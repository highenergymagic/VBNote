//! An SD card in the slot, enough of one for Windows CE to mount it.
//!
//! This exists because the DiskOnChip's flash translation layer will not hand
//! Windows CE a volume it is willing to open, and `\Flash Disk` is what
//! KeySoft waits for before it will finish starting. The ROM already carries
//! a complete block driver for a card — `sdmmc.dll` behind prefix `DSK`, with
//! `fatfs.dll` as its file system — so a card in the slot gets partitioned,
//! formatted and mounted by stock Microsoft code that already works.
//!
//! It is not what the real machine does, and the folder it appears under is a
//! polite fiction maintained by one registry patch. It is documented hardware
//! and a published protocol, which the DiskOnChip's on-media format is not.
//!
//! # What a host has to do to get a card talking
//!
//! The card starts idle and answers almost nothing. The sequence, from the
//! SD Physical Layer specification:
//!
//! ```text
//! CMD0                  go idle
//! CMD8   (SD 2.0+)      does the card understand this voltage?
//! CMD55 + ACMD41        start initialisation; repeat until the card says ready
//! CMD2                  send CID, and move to identification
//! CMD3                  take a relative address
//! CMD9                  send CSD, which is where the capacity is
//! CMD7                  select this card
//! CMD16                 set the block length
//! CMD17 / CMD24         read / write one block
//! ```
//!
//! Responses are byte strings here rather than integers, because that is what
//! the controller shifts out and what the host reassembles: the first byte is
//! the command index, the last is a CRC, and the payload is in between.

/// Bytes in a block. Every SD card uses this regardless of capacity.
pub const BLOCK: usize = 512;

/// The card's operating conditions: 3.2-3.4 V and powered up.
///
/// Bit 31 is the "not busy" flag a host spins on through `ACMD41`, and bit 30
/// says the card addresses blocks rather than bytes.
const OCR_READY: u32 = 0x8000_0000;
const OCR_VOLTAGE_WINDOW: u32 = 0x00FF_8000;

/// Card states, from the specification's state diagram. Only the ones this
/// model can actually be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Ready,
    Identification,
    Standby,
    Transfer,
    SendingData,
    ReceivingData,
}

/// What the card is doing with the data lines, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    None,
    Reading,
    Writing,
}

pub struct SdCard {
    /// Contents, `BLOCK` bytes per block.
    pub data: Vec<u8>,
    pub state: State,
    /// Relative card address, handed out by `CMD3`.
    pub rca: u16,
    /// Set once `CMD55` has been seen, so the next command is an `ACMD`.
    app_cmd: bool,
    /// How many times `ACMD41` has been asked. A real card takes a moment to
    /// power up and a host that never sees "busy" is a host whose retry loop
    /// is never exercised.
    init_polls: u32,
    /// Block length set by `CMD16`. Always `BLOCK` in practice.
    pub block_len: usize,
    /// Byte offset of the transfer in progress.
    pub position: usize,
    pub transfer: Transfer,
    /// Set for `CMD18`/`CMD25`, which run until `CMD12` stops them. A
    /// single-block transfer ends by itself.
    pub multi_block: bool,
    /// Bytes a register command puts on the data lines. `ACMD51` and
    /// `ACMD13` answer with a short response *and* a small block of data,
    /// and a host that asked for data waits for it however good the
    /// response was.
    data_out: std::collections::VecDeque<u8>,
    /// Set when anything has been written, so an untouched card is not saved
    /// over an image that already exists.
    pub dirty: bool,
    /// Blocks written since the last flush, in order.
    ///
    /// The image is the user's data -- documents they have typed -- and
    /// writing it only when the emulator exits tidily means any other ending
    /// loses everything since the card was loaded. Writing the whole image
    /// often is not the answer either: it is over a hundred megabytes and
    /// almost none of it changes. So the card remembers which blocks moved and
    /// the runner writes just those, which is small enough to do every few
    /// seconds.
    dirty_blocks: std::collections::BTreeSet<usize>,
}

impl SdCard {
    /// A blank card of `capacity` bytes, rounded down to whole blocks.
    pub fn new(capacity: usize) -> Self {
        let blocks = capacity / BLOCK;
        SdCard {
            // A fresh card reads as zeroes, not as erased flash: the wear and
            // the erase block are the controller's business, not the host's.
            data: vec![0u8; blocks * BLOCK],
            state: State::Idle,
            multi_block: false,
            rca: 0,
            app_cmd: false,
            init_polls: 0,
            block_len: BLOCK,
            position: 0,
            transfer: Transfer::None,
            data_out: std::collections::VecDeque::new(),
            dirty: false,
            dirty_blocks: std::collections::BTreeSet::new(),
        }
    }

    pub fn from_image(raw: Vec<u8>) -> Self {
        let mut c = SdCard::new(raw.len());
        let n = raw.len().min(c.data.len());
        c.data[..n].copy_from_slice(&raw[..n]);
        c
    }

    pub fn blocks(&self) -> usize {
        self.data.len() / BLOCK
    }

    /// The controller has moved the last byte of a transfer.
    ///
    /// A single-block transfer ends here, and the card goes back to `tran` on
    /// its own: `CMD24` takes it to `rcv` for the data and `prg` while it
    /// writes, and it is in `tran` again by the time the host asks. A
    /// multi-block one stays where it is until `CMD12` stops it.
    ///
    /// Leaving this out is what hung the machine. The driver finishes a write
    /// and then polls `CMD13` until the card says `tran` — with the state
    /// stuck at `rcv` it asked 39,000 times, five milliseconds apart, and
    /// KeySoft never saved a setting.
    pub fn transfer_finished(&mut self) {
        if self.multi_block {
            return;
        }
        self.transfer = Transfer::None;
        if matches!(self.state, State::SendingData | State::ReceivingData) {
            self.state = State::Transfer;
        }
    }

    /// The 32-bit card status that every `R1` response carries.
    fn status(&self) -> u32 {
        let state = match self.state {
            State::Idle => 0,
            State::Ready => 1,
            State::Identification => 2,
            State::Standby => 3,
            State::Transfer => 4,
            State::SendingData => 5,
            State::ReceivingData => 6,
        };
        // Bit 8 is READY_FOR_DATA, and the state sits in bits 12:9.
        (state << 9) | (1 << 8)
    }

    /// The card identification register: who made this card.
    fn cid(&self) -> [u8; 16] {
        let mut r = [0u8; 16];
        r[0] = 0x00; // manufacturer
        r[1] = b'V';
        r[2] = b'N';
        r[3..8].copy_from_slice(b"VIBEn");
        r[8] = 0x10; // revision 1.0
        r[9..13].copy_from_slice(&0x0000_0001u32.to_be_bytes()); // serial
        r[13] = 0x00;
        r[14] = 0x12; // manufacturing date
        r[15] = 0x01; // CRC7 + end bit, which nothing here checks
        r
    }

    /// The card specific data register, **version 1**, where the capacity
    /// lives.
    ///
    /// Version 1 states the size the long way round:
    ///
    /// ```text
    /// capacity = (C_SIZE + 1) * 2^(C_SIZE_MULT + 2) * 2^READ_BL_LEN
    /// ```
    ///
    /// With `READ_BL_LEN` 9 and `C_SIZE_MULT` 7 that is `(C_SIZE + 1) * 256 KB`,
    /// which reaches 1 GB - further than this machine ever had. The fields sit
    /// across byte boundaries, which is why this is built a field at a time
    /// rather than written out as a table.
    fn csd(&self) -> [u8; 16] {
        const READ_BL_LEN: u32 = 9;
        const C_SIZE_MULT: u32 = 7;
        let unit = 1u64 << (C_SIZE_MULT + 2 + READ_BL_LEN);
        let c_size = ((self.data.len() as u64 / unit).saturating_sub(1)) as u32;

        let mut r = [0u8; 16];
        r[0] = 0x00; // CSD version 1.0
        r[1] = 0x26; // TAAC
        r[2] = 0x00; // NSAC
        r[3] = 0x32; // TRAN_SPEED, 25 MHz
        r[4] = 0x5B; // CCC, high bits
        r[5] = 0x50 | (READ_BL_LEN as u8 & 0x0F);
        r[6] = ((c_size >> 10) & 0x03) as u8;
        r[7] = ((c_size >> 2) & 0xFF) as u8;
        // C_SIZE's bottom two bits, then the read current limits.
        r[8] = (((c_size & 0x03) as u8) << 6) | (5 << 3) | 5;
        // Write current limits, then C_SIZE_MULT's top two bits.
        r[9] = (5 << 5) | (5 << 2) | ((C_SIZE_MULT >> 1) & 0x03) as u8;
        // C_SIZE_MULT's bottom bit, erase-block-enable, and the sector size.
        r[10] = (((C_SIZE_MULT & 1) as u8) << 7) | 0x40 | 0x3F;
        r[11] = 0x80; // the sector size's last bit, and no write-protect groups
        r[12] = 0x0A; // R2W_FACTOR, and WRITE_BL_LEN's top bits
        r[13] = 0x40; // WRITE_BL_LEN's bottom bits
        r[14] = 0x00;
        r[15] = 0x01; // CRC7 and the end bit, which nothing here checks
        r
    }

    /// The capacity this CSD describes, so a test can check the arithmetic a
    /// host will do rather than the bytes it will read.
    /// The blocks written since this was last asked, as runs of consecutive
    /// blocks: `(first block, how many)`.
    ///
    /// Runs rather than blocks because a file being written touches a stretch
    /// of the card at once, and one seek and one write for a stretch is worth
    /// having over five hundred of each.
    pub fn take_dirty_runs(&mut self) -> Vec<(usize, usize)> {
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for block in std::mem::take(&mut self.dirty_blocks) {
            match runs.last_mut() {
                Some((start, len)) if *start + *len == block => *len += 1,
                _ => runs.push((block, 1)),
            }
        }
        runs
    }

    /// Whether anything is waiting to be written out.
    pub fn has_unflushed(&self) -> bool {
        !self.dirty_blocks.is_empty()
    }

    /// The bytes of one block, for writing to the image.
    pub fn block_bytes(&self, block: usize, count: usize) -> &[u8] {
        let from = block * BLOCK;
        let to = (from + count * BLOCK).min(self.data.len());
        &self.data[from.min(self.data.len())..to]
    }

    pub fn block_size(&self) -> usize {
        BLOCK
    }

    pub fn csd_capacity(&self) -> u64 {
        let r = self.csd();
        let read_bl_len = (r[5] & 0x0F) as u32;
        let c_size = (((r[6] & 0x03) as u32) << 10) | ((r[7] as u32) << 2) | ((r[8] >> 6) as u32);
        let c_size_mult = (((r[9] & 0x03) as u32) << 1) | ((r[10] >> 7) as u32);
        (c_size as u64 + 1) * (1u64 << (c_size_mult + 2)) * (1u64 << read_bl_len)
    }

    /// Build a 48-bit response: index, four bytes of payload, CRC.
    fn r1(&self, index: u8, payload: u32) -> Vec<u8> {
        let mut v = vec![index & 0x3F];
        v.extend_from_slice(&payload.to_be_bytes());
        v.push(0x01);
        v
    }

    /// Build a 136-bit response, which carries a whole register.
    ///
    /// The first byte is `0x3F` rather than the command index — a long
    /// response has no index to send back.
    fn r2(&self, reg: [u8; 16]) -> Vec<u8> {
        let mut v = vec![0x3Fu8];
        v.extend_from_slice(&reg);
        v
    }

    /// Run one command, returning the response the controller should shift
    /// out, or `None` if the card stays silent.
    pub fn command(&mut self, cmd: u8, arg: u32) -> Option<Vec<u8>> {
        let app = std::mem::take(&mut self.app_cmd);
        if app {
            return self.app_command(cmd, arg);
        }
        match cmd {
            0 => {
                // GO_IDLE_STATE. No response, and everything resets.
                self.state = State::Idle;
                self.transfer = Transfer::None;
                self.data_out.clear();
                None
            }
            2 => {
                // ALL_SEND_CID.
                self.state = State::Identification;
                Some(self.r2(self.cid()))
            }
            1 => {
                // SEND_OP_COND, the MMC form of ACMD41. Same busy-then-ready
                // dance, and the same OCR.
                self.init_polls += 1;
                let ready = self.init_polls >= 2;
                if ready {
                    self.state = State::Ready;
                }
                let ocr = OCR_VOLTAGE_WINDOW | if ready { OCR_READY } else { 0 };
                let mut v = vec![0x3Fu8];
                v.extend_from_slice(&ocr.to_be_bytes());
                v.push(0xFF);
                Some(v)
            }
            3 => {
                // Two protocols share this number and they run opposite ways
                // round. SD's SEND_RELATIVE_ADDR has the card choose an
                // address and hand it back in an R6; MMC's SET_RELATIVE_ADDR
                // has the host name one in the argument and expects a plain
                // status back. The argument tells them apart: a host that is
                // assigning an address has put it there.
                self.state = State::Standby;
                let assigned = (arg >> 16) as u16;
                if assigned != 0 {
                    self.rca = assigned;
                    Some(self.r1(3, self.status()))
                } else {
                    self.rca = 0x0001;
                    let payload = ((self.rca as u32) << 16) | (self.status() & 0xFFFF);
                    Some(self.r1(3, payload))
                }
            }
            6 => Some(self.r1(6, self.status())), // SWITCH_FUNC, accepted and ignored
            7 => {
                // SELECT/DESELECT_CARD.
                self.state = if (arg >> 16) as u16 == self.rca {
                    State::Transfer
                } else {
                    State::Standby
                };
                Some(self.r1(7, self.status()))
            }
            8 => {
                // SEND_IF_COND. Echo the check pattern back to say this card
                // understands version 2 and the supplied voltage.
                Some(self.r1(8, arg & 0xFFF))
            }
            9 => Some(self.r2(self.csd())),  // SEND_CSD
            10 => Some(self.r2(self.cid())), // SEND_CID
            12 => {
                // STOP_TRANSMISSION.
                self.transfer = Transfer::None;
                self.multi_block = false;
                self.state = State::Transfer;
                Some(self.r1(12, self.status()))
            }
            13 => Some(self.r1(13, self.status())), // SEND_STATUS
            16 => {
                // SET_BLOCKLEN.
                self.block_len = arg as usize;
                Some(self.r1(16, self.status()))
            }
            17 | 18 => {
                // READ_SINGLE_BLOCK / READ_MULTIPLE_BLOCK. Without the high
                // capacity bit in the OCR the argument is a byte offset.
                self.position = arg as usize;
                self.transfer = Transfer::Reading;
                self.multi_block = cmd == 18;
                self.state = State::SendingData;
                Some(self.r1(cmd, self.status()))
            }
            24 | 25 => {
                // WRITE_BLOCK / WRITE_MULTIPLE_BLOCK, byte-addressed too.
                self.position = arg as usize;
                self.transfer = Transfer::Writing;
                self.multi_block = cmd == 25;
                self.state = State::ReceivingData;
                Some(self.r1(cmd, self.status()))
            }
            55 => {
                // APP_CMD: the next command is an ACMD.
                self.app_cmd = true;
                Some(self.r1(55, self.status() | (1 << 5)))
            }
            _ => Some(self.r1(cmd, self.status())),
        }
    }

    fn app_command(&mut self, cmd: u8, _arg: u32) -> Option<Vec<u8>> {
        match cmd {
            41 => {
                // SD_SEND_OP_COND. Report busy the first few times so the
                // host's wait loop runs the way it would against a real card,
                // then come up ready.
                self.init_polls += 1;
                let ready = self.init_polls >= 2;
                if ready {
                    self.state = State::Ready;
                }
                let ocr = OCR_VOLTAGE_WINDOW | if ready { OCR_READY } else { 0 };
                // An R3 carries the OCR with no index and no real CRC.
                let mut v = vec![0x3Fu8];
                v.extend_from_slice(&ocr.to_be_bytes());
                v.push(0xFF);
                Some(v)
            }
            6 => Some(self.r1(6, self.status())), // SET_BUS_WIDTH
            13 => {
                // SD_STATUS: 64 bytes over the data lines. Zeroes describe a
                // card with nothing unusual about it, which this is.
                self.data_out.extend(std::iter::repeat_n(0u8, 64));
                self.transfer = Transfer::Reading;
                Some(self.r1(13, self.status()))
            }
            51 => {
                // SEND_SCR: eight bytes over the data lines.
                self.data_out.extend(Self::scr());
                self.transfer = Transfer::Reading;
                Some(self.r1(51, self.status()))
            }
            other => Some(self.r1(other, self.status())),
        }
    }

    /// The SD configuration register: which bus widths and spec version.
    fn scr() -> [u8; 8] {
        // Spec version 2.00, and both one- and four-bit buses supported.
        [0x02, 0x05, 0, 0, 0, 0, 0, 0]
    }

    /// Whether the card has data waiting on the lines. The host polls the
    /// controller's receive-FIFO bit before each read, and a FIFO that never
    /// says it has anything is one the host stops reading from.
    pub fn has_data(&self) -> bool {
        !self.data_out.is_empty() || self.transfer == Transfer::Reading
    }

    /// Next byte the card puts on the data lines.
    pub fn read_byte(&mut self) -> u8 {
        if let Some(b) = self.data_out.pop_front() {
            return b;
        }
        if self.transfer != Transfer::Reading {
            return 0xFF;
        }
        let b = self.data.get(self.position).copied().unwrap_or(0);
        self.position += 1;
        b
    }

    /// A byte the host has clocked in.
    pub fn write_byte(&mut self, b: u8) {
        if self.transfer != Transfer::Writing {
            return;
        }
        if let Some(slot) = self.data.get_mut(self.position) {
            *slot = b;
            self.dirty = true;
            self.dirty_blocks.insert(self.position / BLOCK);
        }
        self.position += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writing marks the block, so the runner knows what to write out without
    /// comparing the whole image.
    #[test]
    fn a_write_marks_its_own_block() {
        let mut c = SdCard::new(64 * 1024);
        assert!(!c.has_unflushed());
        c.command(24, 0);
        for b in 0..4u8 {
            c.write_byte(b);
        }
        assert!(c.has_unflushed());
        assert_eq!(c.take_dirty_runs(), vec![(0, 1)]);
        assert!(!c.has_unflushed(), "asking clears them");
    }

    /// Consecutive blocks come back as one run, because a file lands on a
    /// stretch of the card and one write beats five hundred.
    #[test]
    fn consecutive_blocks_coalesce() {
        let mut c = SdCard::new(64 * 1024);
        let block = c.block_size();
        c.command(24, 0);
        // Three blocks in a row, then a gap, then one more.
        for i in 0..(block * 3) {
            c.position = i;
            c.write_byte(0xAB);
        }
        c.position = block * 10;
        c.write_byte(0xCD);
        assert_eq!(c.take_dirty_runs(), vec![(0, 3), (10, 1)]);
    }

    /// A run has to hand back exactly the bytes it names, or a flush writes
    /// the wrong part of the image -- which would corrupt a card rather than
    /// merely fail to save it.
    #[test]
    fn a_run_hands_back_its_own_bytes() {
        let mut c = SdCard::new(64 * 1024);
        let block = c.block_size();
        c.command(24, 0);
        c.position = block * 2;
        c.write_byte(0x5A);
        let runs = c.take_dirty_runs();
        assert_eq!(runs, vec![(2, 1)]);
        let bytes = c.block_bytes(2, 1);
        assert_eq!(bytes.len(), block);
        assert_eq!(bytes[0], 0x5A);
    }

    /// Asking for more than the card has must not run off the end.
    #[test]
    fn a_run_past_the_end_is_clamped() {
        let c = SdCard::new(64 * 1024);
        let blocks = c.blocks();
        assert!(c.block_bytes(blocks - 1, 4).len() <= 4 * c.block_size());
        assert!(c.block_bytes(blocks, 1).is_empty());
    }

    /// Walk the identification sequence a host actually performs, and check
    /// the card ends up somewhere it can be read from.
    fn bring_up(c: &mut SdCard) {
        c.command(0, 0);
        c.command(8, 0x1AA);
        loop {
            c.command(55, 0);
            let r = c.command(41, 0x4030_0000).unwrap();
            let ocr = u32::from_be_bytes([r[1], r[2], r[3], r[4]]);
            if ocr & OCR_READY != 0 {
                break;
            }
        }
        c.command(2, 0);
        c.command(3, 0);
        c.command(9, (c.rca as u32) << 16);
        c.command(7, (c.rca as u32) << 16);
        c.command(16, BLOCK as u32);
    }

    #[test]
    fn a_host_can_bring_the_card_up_to_transfer_state() {
        let mut c = SdCard::new(1 << 20);
        bring_up(&mut c);
        assert_eq!(c.state, State::Transfer);
        assert_eq!(c.rca, 1, "the card has an address to be selected by");
    }

    /// The host spins on bit 31 of the OCR, so a card that is ready on the
    /// very first ask never exercises that loop.
    #[test]
    fn the_card_reports_busy_before_it_reports_ready() {
        let mut c = SdCard::new(1 << 20);
        c.command(0, 0);
        c.command(55, 0);
        let first = c.command(41, 0).unwrap();
        assert_eq!(u32::from_be_bytes([first[1], first[2], first[3], first[4]]) & OCR_READY, 0);
        c.command(55, 0);
        let then = c.command(41, 0).unwrap();
        assert_ne!(u32::from_be_bytes([then[1], then[2], then[3], then[4]]) & OCR_READY, 0);
    }

    /// MMC assigns the address from the host side; SD has the card pick one.
    /// Answering the MMC form with SD's response leaves the host waiting.
    #[test]
    fn the_relative_address_command_follows_whichever_protocol_asked() {
        let mut mmc = SdCard::new(1 << 20);
        mmc.command(3, 0x0007_0000);
        assert_eq!(mmc.rca, 7, "MMC names the address and the card takes it");

        let mut sd = SdCard::new(1 << 20);
        let r = sd.command(3, 0).unwrap();
        assert_eq!(sd.rca, 1, "SD has the card choose");
        assert_eq!(u16::from_be_bytes([r[1], r[2]]), 1, "and report it back");
    }

    #[test]
    fn a_block_written_reads_back() {
        let mut c = SdCard::new(1 << 20);
        bring_up(&mut c);
        c.command(24, 3 * BLOCK as u32);
        for i in 0..BLOCK {
            c.write_byte((i % 251) as u8);
        }
        c.command(17, 3 * BLOCK as u32);
        let got: Vec<u8> = (0..BLOCK).map(|_| c.read_byte()).collect();
        assert_eq!(got, (0..BLOCK).map(|i| (i % 251) as u8).collect::<Vec<_>>());
        assert!(c.dirty);
    }

    /// A card without the high-capacity bit is addressed in bytes. Treating
    /// the argument as a block number puts every access 512 times too far in.
    #[test]
    fn addresses_are_bytes_rather_than_blocks() {
        let mut c = SdCard::new(1 << 20);
        bring_up(&mut c);
        c.command(17, 2 * BLOCK as u32);
        assert_eq!(c.position, 2 * BLOCK);
    }

    /// The capacity the host reads out of the CSD has to be the capacity the
    /// card has, or the file system is built to the wrong size. This runs the
    /// arithmetic a host does against the bytes it reads.
    #[test]
    fn the_csd_reports_the_real_capacity() {
        for mb in [16usize, 64, 128, 256] {
            let c = SdCard::new(mb * 1024 * 1024);
            assert_eq!(c.csd_capacity(), c.data.len() as u64, "{mb} MB");
        }
    }

    /// A 2003 driver predates SDHC by three years. Announcing a version 2 CSD,
    /// or the high-capacity bit that goes with it, describes a card it cannot
    /// parse - and it formatted a 128 MB card as a 96 KB one.
    #[test]
    fn the_card_describes_itself_the_way_a_2003_host_expects() {
        let mut c = SdCard::new(128 * 1024 * 1024);
        assert_eq!(c.csd()[0] >> 6, 0, "CSD version 1");
        c.command(55, 0);
        let r = c.command(41, 0).unwrap();
        let ocr = u32::from_be_bytes([r[1], r[2], r[3], r[4]]);
        assert_eq!(ocr & 0x4000_0000, 0, "not a high capacity card");
    }

    #[test]
    fn a_long_response_carries_a_whole_register() {
        let mut c = SdCard::new(1 << 20);
        let r = c.command(2, 0).unwrap();
        assert_eq!(r.len(), 17, "one lead byte and sixteen of register");
        assert_eq!(r[0], 0x3F, "a long response has no command index");
    }

    /// A host that asked for data waits for data, however good the response
    /// was. SEND_SCR answers with both.
    #[test]
    fn the_configuration_register_comes_back_over_the_data_lines() {
        let mut c = SdCard::new(1 << 20);
        bring_up(&mut c);
        c.command(55, 0);
        c.command(51, 0);
        let got: Vec<u8> = (0..8).map(|_| c.read_byte()).collect();
        assert_eq!(got[0], 0x02, "SD specification version 2");
        assert_ne!(got[1] & 0x04, 0, "and a four-bit bus on offer");
    }

    #[test]
    fn reading_without_a_read_command_does_not_hand_out_data() {
        let mut c = SdCard::new(1 << 20);
        bring_up(&mut c);
        assert_eq!(c.read_byte(), 0xFF, "the data lines are idle");
    }
    /// After a single-block write the card must find its own way back to
    /// `tran`, because nothing else will take it there: the driver does not
    /// send `CMD12` after `CMD24`, it polls `CMD13` until the card says it is
    /// ready. This is the hang that stopped KeySoft ever saving a setting.
    #[test]
    fn a_single_block_write_leaves_the_card_ready_again() {
        let mut c = SdCard::new(64 * 1024);
        c.state = State::Transfer;
        c.command(24, 0);
        assert_eq!(c.state, State::ReceivingData, "the data phase has started");
        c.transfer_finished();
        assert_eq!(c.state, State::Transfer, "and the card is ready for the next command");
        // Which is what the driver actually looks at.
        let status = c.status();
        assert_eq!((status >> 9) & 0xF, 4, "CURRENT_STATE reads as tran");
    }

    /// A multi-block transfer is the other way round: it runs until `CMD12`
    /// stops it, so finishing a block must not end it early.
    #[test]
    fn a_multi_block_write_runs_until_it_is_stopped() {
        let mut c = SdCard::new(64 * 1024);
        c.state = State::Transfer;
        c.command(25, 0);
        c.transfer_finished();
        assert_eq!(c.state, State::ReceivingData, "still receiving");
        c.command(12, 0);
        assert_eq!(c.state, State::Transfer, "CMD12 is what ends it");
    }

    /// The same for reads, which have the same shape.
    #[test]
    fn a_single_block_read_leaves_the_card_ready_again() {
        let mut c = SdCard::new(64 * 1024);
        c.state = State::Transfer;
        c.command(17, 0);
        assert_eq!(c.state, State::SendingData);
        c.transfer_finished();
        assert_eq!(c.state, State::Transfer);
    }

}
