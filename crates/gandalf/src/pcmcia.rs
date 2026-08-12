//! The PCMCIA/CompactFlash socket, and a CompactFlash card to put in it.
//!
//! This is how files get on and off the machine. The firmware already has the
//! whole stack -- `pcmcia.dll` is loaded and its `DllMain` runs on every boot,
//! `atadisk.dll` and `mspart.dll` sit in ROM waiting, and the storage manager
//! has a `CompactFlash` profile that mounts a card as `\CompactFlash`. There
//! is nothing to add to the guest: a card has to appear, and CE does the rest.
//!
//! # Where the windows are
//!
//! Read out of `pcmcia.dll`'s own `.data` at `0x022ce150`, which is a table of
//! (kind, size, base) triples rather than anything inferred:
//!
//! | window | socket 0 | socket 1 |
//! |---|---|---|
//! | common memory | `0x2000_0000` | `0x3000_0000` |
//! | attribute memory | `0x2800_0000` | `0x3800_0000` |
//! | I/O | `0x2C00_0000` | `0x3C00_0000` |
//!
//! So each socket owns 256 MB, split into three spaces by bits 27:26. Only
//! socket 0 is wired here; socket 1 reads as an empty slot.
//!
//! # Attribute memory is every other byte
//!
//! PCMCIA attribute space puts its data on the low half of the bus, so byte
//! *n* of the CIS lives at attribute offset *2n* and the odd addresses read
//! back as zero. Getting this wrong does not fail loudly -- card services
//! reads a tuple chain that looks like garbage and simply decides there is no
//! card worth having.
//!
//! # How a card is recognised
//!
//! The ROM registry has `Drivers\PCMCIA\Detect\50` with `Entry =
//! DetectATADisk`, `Dll = ATADISK.DLL`. Card services first tries to match the
//! CIS strings against a named `Drivers\PCMCIA\<mfr>-<product>-<crc>` key,
//! finds nothing for a card nobody has heard of, and falls through to the
//! detect list, where `DetectATADisk` probes the ATA task file. That is why
//! this card does not have to impersonate any particular product: it has to
//! carry a well-formed CIS saying "fixed disk", and answer `IDENTIFY DEVICE`.

use std::collections::BTreeMap;

use pxa270::AccessStat;

/// The three address spaces a PC Card presents, selected by bits 27:26 of the
/// address within the socket's 256 MB region.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Space {
    /// Where the data lives, once the card is configured.
    Common,
    /// The CIS and the configuration registers.
    Attribute,
    /// The ATA task file.
    Io,
}

/// Card space: both sockets, all three windows.
pub const BASE: u32 = 0x2000_0000;
pub const END: u32 = 0x3FFF_FFFF;

/// Split a physical address into socket, space and offset within that space.
pub fn decode(pa: u32) -> Option<(u8, Space, u32)> {
    if !(BASE..=END).contains(&pa) {
        return None;
    }
    let socket = ((pa - BASE) >> 28) as u8;
    let within = pa & 0x0FFF_FFFF;
    let space = match within >> 26 {
        0 | 1 => Space::Common,
        2 => Space::Attribute,
        _ => Space::Io,
    };
    Some((socket, space, within & 0x03FF_FFFF))
}

/// The socket, with or without a card in it.
#[derive(Default)]
pub struct Socket {
    pub card: Option<Card>,
    /// Every access, by (space, offset), for finding out what the driver
    /// wanted when it did not get it.
    pub log: BTreeMap<(Space, u32), AccessStat>,
    /// Set when a card is inserted or removed and not yet reported to the
    /// guest, so the runner can decide how to signal it.
    pub changed: bool,
}

impl Socket {
    pub fn new() -> Self {
        Self::default()
    }

    /// Put a card in. Returns what was there before.
    pub fn insert(&mut self, card: Card) -> Option<Card> {
        self.changed = true;
        self.card.replace(card)
    }

    pub fn eject(&mut self) -> Option<Card> {
        self.changed = true;
        self.card.take()
    }

    pub fn occupied(&self) -> bool {
        self.card.is_some()
    }

    fn note(&mut self, space: Space, off: u32, pc: u32, write: bool, val: u32) {
        let e = self
            .log
            .entry((space, off))
            .or_insert(AccessStat { first_pc: pc, ..Default::default() });
        if write {
            e.writes += 1;
            e.last_value = val;
        } else {
            e.reads += 1;
        }
    }

    pub fn read(&mut self, pa: u32, width: u32, pc: u32) -> u32 {
        let Some((socket, space, off)) = decode(pa) else {
            return !0;
        };
        self.note(space, off, pc, false, 0);
        if socket != 0 {
            return !0;
        }
        let Some(card) = self.card.as_mut() else {
            // An empty socket floats high, which is what tells card services
            // there is nothing there.
            return !0;
        };
        card.read(space, off, width)
    }

    pub fn write(&mut self, pa: u32, val: u32, width: u32, pc: u32) {
        let Some((socket, space, off)) = decode(pa) else {
            return;
        };
        self.note(space, off, pc, true, val);
        if socket != 0 {
            return;
        }
        if let Some(card) = self.card.as_mut() {
            card.write(space, off, val, width);
        }
    }
}

// ---------------------------------------------------------------------------
// The card
// ---------------------------------------------------------------------------

/// Where the card's configuration registers sit in attribute space, in card
/// bytes. Named in `CISTPL_CONFIG` so the driver can find them.
const CONFIG_BASE: u16 = 0x0200;

/// Sectors are 512 bytes and nothing here changes that.
pub const SECTOR: usize = 512;

/// A CompactFlash card: a CIS to be recognised by, configuration registers to
/// be turned on through, and an ATA task file to be read and written.
pub struct Card {
    cis: Vec<u8>,
    /// Configuration Option Register: the driver writes here to choose a
    /// configuration and to reset the card.
    cor: u8,
    /// The disk itself.
    pub data: Vec<u8>,
    ata: Ata,
}

impl Card {
    /// A card of `sectors` 512-byte sectors, all zero.
    pub fn blank(sectors: u32) -> Card {
        Card::with_data(vec![0; sectors as usize * SECTOR])
    }

    pub fn with_data(data: Vec<u8>) -> Card {
        let sectors = (data.len() / SECTOR) as u32;
        Card {
            cis: cis(),
            cor: 0,
            data,
            ata: Ata::new(sectors),
        }
    }

    pub fn sectors(&self) -> u32 {
        (self.data.len() / SECTOR) as u32
    }

    fn read(&mut self, space: Space, off: u32, width: u32) -> u32 {
        match space {
            // Byte n of the card lives at offset 2n, and the odd bytes are
            // not driven.
            Space::Attribute => {
                if off & 1 != 0 {
                    return 0;
                }
                let n = (off / 2) as u16;
                if n == CONFIG_BASE {
                    return self.cor as u32;
                }
                self.cis.get(n as usize).copied().unwrap_or(0xFF) as u32
            }
            Space::Io => self.ata.read(off, width, &self.data),
            // Common memory is not used in the I/O configuration a CF card
            // is driven in here.
            Space::Common => !0,
        }
    }

    fn write(&mut self, space: Space, off: u32, val: u32, width: u32) {
        match space {
            Space::Attribute => {
                if off & 1 != 0 {
                    return;
                }
                if (off / 2) as u16 == CONFIG_BASE {
                    self.cor = val as u8;
                    // Bit 7 is SRESET: it puts the ATA side back to its
                    // power-on state without disturbing the disk.
                    if self.cor & 0x80 != 0 {
                        self.ata.reset();
                    }
                }
            }
            Space::Io => self.ata.write(off, val, width, &mut self.data),
            Space::Common => {}
        }
    }
}

/// The card's CIS: the tuple chain card services walks to find out what this
/// is. Deliberately plain -- a fixed-disk function with one I/O
/// configuration -- because being unremarkable is what routes it to
/// `DetectATADisk` rather than to some product-specific driver.
fn cis() -> Vec<u8> {
    let mut c: Vec<u8> = Vec::new();

    let tuple = |code: u8, body: &[u8], c: &mut Vec<u8>| {
        c.push(code);
        c.push(body.len() as u8);
        c.extend_from_slice(body);
    };

    // CISTPL_DEVICE: no common-memory device.
    tuple(0x01, &[0x00, 0xFF], &mut c);

    // CISTPL_VERS_1: major 4, minor 1, then NUL-terminated strings and 0xFF.
    let mut vers = vec![0x04, 0x01];
    for s in ["Fractal Microsystems", "VBNote Storage Card", "", ""] {
        vers.extend_from_slice(s.as_bytes());
        vers.push(0);
    }
    vers.push(0xFF);
    tuple(0x15, &vers, &mut c);

    // CISTPL_FUNCID: function 4 is a fixed disk; 0x01 means it may be
    // initialised at POST.
    tuple(0x21, &[0x04, 0x01], &mut c);

    // CISTPL_FUNCE: disk-function extensions. First the interface (ATA),
    // then the basic disk features.
    tuple(0x22, &[0x01, 0x01], &mut c);
    tuple(0x22, &[0x02, 0x0C, 0x0F], &mut c);

    // CISTPL_CONFIG: one byte of register address, one of mask, then the
    // base address of the configuration registers in card bytes, then the
    // register presence mask (COR only).
    tuple(
        0x1A,
        &[
            0x01,
            0x01,
            (CONFIG_BASE & 0xFF) as u8,
            (CONFIG_BASE >> 8) as u8,
            0x01,
        ],
        &mut c,
    );

    // CISTPL_CFTABLE_ENTRY: configuration 1, the default, 16 contiguous I/O
    // ports decoded on 4 address lines, one interrupt level.
    tuple(
        0x1B,
        &[
            0xC1, // index 1, default entry, interface byte follows
            0x41, // I/O interface, waits not required
            0x99, // I/O space, IRQ, and a power description follow
            0x01, // Vcc: nominal only
            0x55, // 5.0 V
            0x64, // 16 ports, 4 address lines decoded
            0xF0, 0xFF, // level-mode interrupt, any level
            0xFF, // no more
        ],
        &mut c,
    );

    // CISTPL_NO_LINK, then the end of the chain.
    tuple(0x14, &[], &mut c);
    c.push(0xFF);
    c
}

// ---------------------------------------------------------------------------
// The ATA task file
// ---------------------------------------------------------------------------

// BSY (0x80) is deliberately absent: nothing here takes any time, so the card
// is never busy. A driver that waits for BSY to clear finds it already clear.
const STATUS_DRDY: u8 = 0x40;
const STATUS_DRQ: u8 = 0x08;
const STATUS_ERR: u8 = 0x01;

/// What the card is in the middle of doing.
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    /// Handing sectors to the host.
    Reading,
    /// Taking sectors from it.
    Writing,
}

struct Ata {
    sectors: u32,
    features: u8,
    count: u8,
    lba: [u8; 4],
    status: u8,
    error: u8,
    /// The sector being handed over a halfword at a time.
    buffer: Vec<u8>,
    at: usize,
    phase: Phase,
    /// Sectors still to go in a multi-sector command.
    remaining: u8,
}

impl Ata {
    fn new(sectors: u32) -> Ata {
        Ata {
            sectors,
            features: 0,
            count: 1,
            lba: [0; 4],
            status: STATUS_DRDY,
            error: 0,
            buffer: Vec::new(),
            at: 0,
            phase: Phase::Idle,
            remaining: 0,
        }
    }

    fn reset(&mut self) {
        self.status = STATUS_DRDY;
        self.error = 0;
        self.buffer.clear();
        self.at = 0;
        self.phase = Phase::Idle;
        self.remaining = 0;
    }

    /// The sector the LBA registers currently point at.
    fn lba(&self) -> u32 {
        u32::from(self.lba[0])
            | u32::from(self.lba[1]) << 8
            | u32::from(self.lba[2]) << 16
            | u32::from(self.lba[3] & 0x0F) << 24
    }

    fn set_lba(&mut self, lba: u32) {
        self.lba[0] = lba as u8;
        self.lba[1] = (lba >> 8) as u8;
        self.lba[2] = (lba >> 16) as u8;
        self.lba[3] = (self.lba[3] & 0xF0) | ((lba >> 24) as u8 & 0x0F);
    }

    fn read(&mut self, off: u32, width: u32, disk: &[u8]) -> u32 {
        match off & 0x0F {
            0 => self.take_data(width, disk),
            1 => self.error as u32,
            2 => self.count as u32,
            3 => self.lba[0] as u32,
            4 => self.lba[1] as u32,
            5 => self.lba[2] as u32,
            6 => self.lba[3] as u32,
            7 | 0x0E => self.status as u32,
            0x0D => self.error as u32,
            0x0F => 0x01,
            _ => 0,
        }
    }

    fn write(&mut self, off: u32, val: u32, width: u32, disk: &mut [u8]) {
        match off & 0x0F {
            0 => self.put_data(val, width, disk),
            1 => self.features = val as u8,
            2 => self.count = val as u8,
            3 => self.lba[0] = val as u8,
            4 => self.lba[1] = val as u8,
            5 => self.lba[2] = val as u8,
            6 => self.lba[3] = val as u8,
            7 => self.command(val as u8, disk),
            // Device control: bit 2 is SRST.
            0x0E if val as u8 & 0x04 != 0 => self.reset(),
            _ => {}
        }
    }

    fn command(&mut self, cmd: u8, disk: &[u8]) {
        self.error = 0;
        self.status = STATUS_DRDY;
        match cmd {
            // IDENTIFY DEVICE, and the CF-specific IDENTIFY that some drivers
            // try first.
            0xEC | 0xA1 => {
                self.buffer = self.identify();
                self.begin_read(1);
            }
            // READ SECTOR(S), with and without retry.
            0x20 | 0x21 => {
                let n = if self.count == 0 { 256 } else { self.count as u16 };
                if self.load(disk) {
                    self.begin_read(n as u8);
                }
            }
            // WRITE SECTOR(S).
            0x30 | 0x31 => {
                if self.lba() >= self.sectors {
                    self.fail();
                    return;
                }
                self.remaining = self.count;
                self.buffer = vec![0; SECTOR];
                self.at = 0;
                self.phase = Phase::Writing;
                self.status = STATUS_DRDY | STATUS_DRQ;
            }
            // SET FEATURES, INITIALIZE DEVICE PARAMETERS, CHECK POWER MODE,
            // and the two idle commands: nothing to do, but they must not be
            // reported as failures.
            0xEF | 0x91 | 0xE5 | 0xE1 | 0xE3 | 0x00 => {}
            // EXECUTE DEVICE DIAGNOSTIC: 0x01 means "passed".
            0x90 => self.error = 0x01,
            _ => self.fail(),
        }
    }

    fn fail(&mut self) {
        self.status = STATUS_DRDY | STATUS_ERR;
        // ABRT.
        self.error = 0x04;
        self.phase = Phase::Idle;
    }

    fn begin_read(&mut self, sectors: u8) {
        self.at = 0;
        self.remaining = sectors;
        self.phase = Phase::Reading;
        self.status = STATUS_DRDY | STATUS_DRQ;
    }

    /// Fill the buffer from the sector the LBA registers point at.
    fn load(&mut self, disk: &[u8]) -> bool {
        let lba = self.lba() as usize;
        let at = lba * SECTOR;
        if at + SECTOR > disk.len() {
            self.fail();
            return false;
        }
        self.buffer = disk[at..at + SECTOR].to_vec();
        true
    }

    fn take_data(&mut self, width: u32, disk: &[u8]) -> u32 {
        if self.phase != Phase::Reading {
            return 0;
        }
        let n = width.max(1) as usize;
        let mut out = 0u32;
        for i in 0..n {
            let byte = self.buffer.get(self.at).copied().unwrap_or(0);
            out |= u32::from(byte) << (8 * i);
            self.at += 1;
        }
        if self.at >= self.buffer.len() {
            self.remaining = self.remaining.saturating_sub(1);
            if self.remaining == 0 {
                self.phase = Phase::Idle;
                self.status = STATUS_DRDY;
            } else {
                // On to the next sector.
                let next = self.lba() + 1;
                self.set_lba(next);
                self.at = 0;
                if !self.load(disk) {
                    return out;
                }
                self.status = STATUS_DRDY | STATUS_DRQ;
            }
        }
        out
    }

    fn put_data(&mut self, val: u32, width: u32, disk: &mut [u8]) {
        if self.phase != Phase::Writing {
            return;
        }
        let n = width.max(1) as usize;
        for i in 0..n {
            if self.at < self.buffer.len() {
                self.buffer[self.at] = (val >> (8 * i)) as u8;
                self.at += 1;
            }
        }
        if self.at >= self.buffer.len() {
            let lba = self.lba() as usize;
            let at = lba * SECTOR;
            if at + SECTOR <= disk.len() {
                disk[at..at + SECTOR].copy_from_slice(&self.buffer);
            }
            self.remaining = self.remaining.saturating_sub(1);
            if self.remaining == 0 {
                self.phase = Phase::Idle;
                self.status = STATUS_DRDY;
            } else {
                self.set_lba(lba as u32 + 1);
                self.at = 0;
                self.status = STATUS_DRDY | STATUS_DRQ;
            }
        }
    }

    /// The 256-word answer to IDENTIFY DEVICE.
    fn identify(&self) -> Vec<u8> {
        let mut w = [0u16; 256];
        // Fixed disk, not removable in the ATA sense: a CF card that says it
        // is removable invites the storage manager to treat it as a floppy.
        w[0] = 0x044A;
        let sectors = self.sectors.max(1);
        // A geometry that multiplies out to the right size. CE only uses it
        // when the driver is in CHS mode, which it is not, but it must be
        // consistent or the driver rejects the card.
        let heads = 16u32;
        let spt = 63u32;
        let cyls = (sectors / (heads * spt)).clamp(1, 65535);
        w[1] = cyls as u16;
        w[3] = heads as u16;
        w[6] = spt as u16;
        put_string(&mut w[10..20], "VBNOTE0000000000001");
        w[20] = 3;
        w[21] = 8;
        w[22] = 4;
        put_string(&mut w[23..27], "1.0");
        put_string(&mut w[27..47], "VBNote Storage Card");
        // Up to 16 sectors per READ/WRITE MULTIPLE.
        w[47] = 0x8010;
        // LBA supported, DMA not.
        w[49] = 0x0200;
        w[51] = 0x0200;
        w[53] = 0x0003;
        w[54] = cyls as u16;
        w[55] = heads as u16;
        w[56] = spt as u16;
        w[57] = (sectors & 0xFFFF) as u16;
        w[58] = (sectors >> 16) as u16;
        w[60] = (sectors & 0xFFFF) as u16;
        w[61] = (sectors >> 16) as u16;
        let mut out = Vec::with_capacity(SECTOR);
        for word in w {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out
    }
}

/// ATA strings are byte-swapped within each word and padded with spaces.
fn put_string(words: &mut [u16], s: &str) {
    let bytes = s.as_bytes();
    for (i, word) in words.iter_mut().enumerate() {
        let hi = bytes.get(i * 2).copied().unwrap_or(b' ');
        let lo = bytes.get(i * 2 + 1).copied().unwrap_or(b' ');
        *word = u16::from(hi) << 8 | u16::from(lo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window bases are pcmcia.dll's own, so a mistake here is a mistake
    /// about the firmware rather than about taste.
    #[test]
    fn the_windows_are_where_the_driver_says() {
        assert_eq!(decode(0x2000_0000), Some((0, Space::Common, 0)));
        assert_eq!(decode(0x2800_0000), Some((0, Space::Attribute, 0)));
        assert_eq!(decode(0x2C00_0000), Some((0, Space::Io, 0)));
        assert_eq!(decode(0x3000_0000), Some((1, Space::Common, 0)));
        assert_eq!(decode(0x3800_0000), Some((1, Space::Attribute, 0)));
        assert_eq!(decode(0x3C00_0000), Some((1, Space::Io, 0)));
        assert_eq!(decode(0x1FFF_FFFF), None);
        assert_eq!(decode(0x4000_0000), None);
    }

    #[test]
    fn an_empty_socket_floats_high() {
        let mut s = Socket::new();
        assert_eq!(s.read(0x2800_0000, 1, 0), !0);
        assert!(!s.occupied());
    }

    /// Byte n of the CIS at attribute offset 2n, odd addresses undriven. This
    /// is the mistake that would look like "no card" rather than like a bug.
    #[test]
    fn the_cis_reads_on_even_addresses_only() {
        let mut s = Socket::new();
        s.insert(Card::blank(64));
        let chain = cis();
        for (n, want) in chain.iter().enumerate().take(8) {
            assert_eq!(s.read(0x2800_0000 + n as u32 * 2, 1, 0), *want as u32);
            assert_eq!(s.read(0x2800_0000 + n as u32 * 2 + 1, 1, 0), 0);
        }
    }

    /// Card services walks the chain tuple by tuple. If the lengths do not
    /// add up it stops early and the card is never identified.
    #[test]
    fn the_tuple_chain_walks_to_its_end() {
        let chain = cis();
        let mut at = 0;
        let mut seen = Vec::new();
        while at < chain.len() {
            let code = chain[at];
            if code == 0xFF {
                seen.push(code);
                break;
            }
            let len = chain[at + 1] as usize;
            seen.push(code);
            at += 2 + len;
        }
        assert_eq!(*seen.last().unwrap(), 0xFF, "chain did not end: {seen:02x?}");
        // The tuples that decide what this is.
        assert!(seen.contains(&0x21), "no CISTPL_FUNCID");
        assert!(seen.contains(&0x1A), "no CISTPL_CONFIG");
        assert!(seen.contains(&0x1B), "no CISTPL_CFTABLE_ENTRY");
    }

    /// Function 4 is a fixed disk, which is what sends card services to
    /// DetectATADisk rather than to a modem or a network card.
    #[test]
    fn the_card_says_it_is_a_fixed_disk() {
        let chain = cis();
        let at = chain.windows(2).position(|w| w[0] == 0x21).unwrap();
        assert_eq!(chain[at + 2], 0x04);
    }

    #[test]
    fn identify_reports_the_size_it_was_made_with() {
        let mut card = Card::blank(2048);
        card.write(Space::Io, 7, 0xEC, 1);
        let mut got = Vec::new();
        for _ in 0..SECTOR / 2 {
            got.extend_from_slice(&(card.read(Space::Io, 0, 2) as u16).to_le_bytes());
        }
        let word = |n: usize| u16::from_le_bytes([got[n * 2], got[n * 2 + 1]]);
        let lba = u32::from(word(60)) | u32::from(word(61)) << 16;
        assert_eq!(lba, 2048);
        // And the status has dropped DRQ now the sector has been taken.
        assert_eq!(card.read(Space::Io, 7, 1) as u8 & STATUS_DRQ, 0);
    }

    #[test]
    fn a_sector_written_reads_back() {
        let mut card = Card::blank(64);
        let payload: Vec<u8> = (0..SECTOR).map(|i| (i % 251) as u8).collect();

        card.write(Space::Io, 3, 5, 1); // LBA 5
        card.write(Space::Io, 2, 1, 1); // one sector
        card.write(Space::Io, 7, 0x30, 1); // WRITE SECTORS
        for pair in payload.chunks_exact(2) {
            let v = u16::from_le_bytes([pair[0], pair[1]]);
            card.write(Space::Io, 0, v as u32, 2);
        }

        card.write(Space::Io, 3, 5, 1);
        card.write(Space::Io, 2, 1, 1);
        card.write(Space::Io, 7, 0x20, 1); // READ SECTORS
        let mut got = Vec::new();
        for _ in 0..SECTOR / 2 {
            got.extend_from_slice(&(card.read(Space::Io, 0, 2) as u16).to_le_bytes());
        }
        assert_eq!(got, payload);
        assert_eq!(&card.data[5 * SECTOR..6 * SECTOR], &payload[..]);
    }

    /// Reading past the end must be an error, not a panic and not zeroes:
    /// mspart reads the last sector to find the partition table.
    #[test]
    fn reading_off_the_end_is_an_error() {
        let mut card = Card::blank(8);
        card.write(Space::Io, 3, 99, 1);
        card.write(Space::Io, 7, 0x20, 1);
        let status = card.read(Space::Io, 7, 1) as u8;
        assert_eq!(status & STATUS_ERR, STATUS_ERR);
    }

    /// A multi-sector read walks on by itself; the driver only reads the data
    /// register.
    #[test]
    fn two_sectors_come_out_in_order() {
        let mut card = Card::blank(16);
        card.data[0] = 0xAA;
        card.data[SECTOR] = 0xBB;
        card.write(Space::Io, 3, 0, 1);
        card.write(Space::Io, 2, 2, 1);
        card.write(Space::Io, 7, 0x20, 1);
        let first = card.read(Space::Io, 0, 1) as u8;
        for _ in 1..SECTOR {
            card.read(Space::Io, 0, 1);
        }
        let second = card.read(Space::Io, 0, 1) as u8;
        assert_eq!((first, second), (0xAA, 0xBB));
    }

    #[test]
    fn a_configuration_write_can_reset_the_card() {
        let mut card = Card::blank(16);
        card.write(Space::Io, 7, 0x20, 1);
        card.write(Space::Attribute, CONFIG_BASE as u32 * 2, 0x80, 1);
        assert_eq!(card.read(Space::Io, 7, 1) as u8, STATUS_DRDY);
    }

    #[test]
    fn an_unknown_command_is_refused_rather_than_ignored() {
        let mut card = Card::blank(16);
        card.write(Space::Io, 7, 0x77, 1);
        assert_eq!(card.read(Space::Io, 7, 1) as u8 & STATUS_ERR, STATUS_ERR);
    }
}
