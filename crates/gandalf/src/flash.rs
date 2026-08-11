//! Intel-command-set CFI NOR flash, the 64 MB device on nCS0.
//!
//! Modelled after QEMU's `hw/block/pflash_cfi01.c`. The board wires two 16-bit
//! devices in parallel onto the PXA's 32-bit bus, which is visible in the
//! firmware's very first command write: EBOOT writes `0x00FF00FF`, the same
//! 16-bit "read array" opcode in both halves. Every command and every status
//! or query response is therefore replicated across the two halves, and a
//! query address advances four bytes per device word.

/// Number of parallel devices making up the bank.
const INTERLEAVE: u32 = 2;
/// Bytes per erase block, per device.
const DEVICE_BLOCK_SIZE: usize = 128 * 1024;
/// Bytes per device.
const DEVICE_SIZE: usize = 32 * 1024 * 1024;

// Intel command set.
const CMD_READ_ARRAY: u16 = 0xFF;
const CMD_READ_ID: u16 = 0x90;
const CMD_READ_CFI: u16 = 0x98;
const CMD_READ_STATUS: u16 = 0x70;
const CMD_CLEAR_STATUS: u16 = 0x50;
const CMD_PROGRAM: u16 = 0x40;
const CMD_PROGRAM_ALT: u16 = 0x10;
const CMD_BUFFER_WRITE: u16 = 0xE8;
const CMD_BLOCK_ERASE: u16 = 0x20;
const CMD_LOCK_SETUP: u16 = 0x60;
const CMD_CONFIRM: u16 = 0xD0;

// Status register bits.
const SR_READY: u16 = 1 << 7;
const SR_ERASE_ERROR: u16 = 1 << 5;
const SR_PROGRAM_ERROR: u16 = 1 << 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    ReadArray,
    ReadStatus,
    ReadId,
    ReadCfi,
    ProgramSetup,
    EraseSetup,
    LockSetup,
    /// Buffered write: collecting the count, then the data.
    BufferCount,
    BufferData { addr: u32, remaining: u32 },
    BufferConfirm,
}

pub struct NorFlash {
    pub data: Vec<u8>,
    mode: Mode,
    status: u16,
    /// Per-block lock bits, indexed by bus block.
    locked: Vec<bool>,
    /// Set when the guest has issued any command; useful for diagnostics.
    pub commands_seen: u32,
    /// Erases and programs actually performed.
    pub writes_performed: u32,
}

impl NorFlash {
    pub fn new(size: usize) -> Self {
        let blocks = size / Self::bus_block_size();
        NorFlash {
            data: vec![0xFF; size],
            mode: Mode::ReadArray,
            status: SR_READY,
            locked: vec![false; blocks],
            commands_seen: 0,
            writes_performed: 0,
        }
    }

    /// Erase block size as seen on the bus, which is the per-device block
    /// size multiplied by the interleave.
    pub const fn bus_block_size() -> usize {
        DEVICE_BLOCK_SIZE * INTERLEAVE as usize
    }

    #[inline]
    fn replicate(v: u16) -> u32 {
        (v as u32) | ((v as u32) << 16)
    }

    /// CFI query response for a device-word address.
    fn cfi_word(&self, index: u32) -> u16 {
        // Erase region: 256 blocks of 128 KB, encoded as (count - 1) and
        // (size / 256), both little-endian byte pairs.
        let blocks_per_device = (DEVICE_SIZE / DEVICE_BLOCK_SIZE) as u32 - 1;
        let block_units = (DEVICE_BLOCK_SIZE / 256) as u32;
        let v: u32 = match index {
            0x10 => 0x51, // 'Q'
            0x11 => 0x52, // 'R'
            0x12 => 0x59, // 'Y'
            0x13 => 0x01, // primary command set: Intel/Sharp extended
            0x14 => 0x00,
            0x15 => 0x31, // primary extended table at 0x31
            0x16 => 0x00,
            0x17..=0x1A => 0x00, // no alternate command set
            0x1B => 0x27,        // Vcc min 2.7 V
            0x1C => 0x36,        // Vcc max 3.6 V
            0x1D => 0x00,        // Vpp min
            0x1E => 0x00,        // Vpp max
            0x1F => 0x07,        // typical word program timeout, 2^7 us
            0x20 => 0x07,        // typical buffer write timeout
            0x21 => 0x0A,        // typical block erase timeout, 2^10 ms
            0x22 => 0x00,        // chip erase unsupported
            0x23 => 0x04,        // max word program multiplier
            0x24 => 0x04,        // max buffer write multiplier
            0x25 => 0x04,        // max block erase multiplier
            0x26 => 0x00,
            0x27 => DEVICE_SIZE.trailing_zeros(), // device size, 2^n bytes
            0x28 => 0x02,                         // x8/x16 asynchronous interface
            0x29 => 0x00,
            0x2A => 0x05, // write buffer 2^5 = 32 bytes
            0x2B => 0x00,
            0x2C => 0x01, // one erase block region
            0x2D => blocks_per_device & 0xFF,
            0x2E => (blocks_per_device >> 8) & 0xFF,
            0x2F => block_units & 0xFF,
            0x30 => (block_units >> 8) & 0xFF,
            0x31 => 0x50, // 'P'
            0x32 => 0x52, // 'R'
            0x33 => 0x49, // 'I'
            0x34 => 0x31, // major version '1'
            0x35 => 0x31, // minor version '1'
            0x36..=0x3F => 0x00,
            _ => 0x00,
        };
        v as u16
    }

    /// Read identifier response for a device-word address.
    fn id_word(&self, index: u32) -> u16 {
        match index & 0xFF {
            0x00 => 0x0089, // Intel
            0x01 => 0x001D, // 28F256J3, 256 Mbit
            0x02 => {
                let block = (index as usize / (Self::bus_block_size() / 4)) % self.locked.len();
                self.locked[block] as u16
            }
            _ => 0,
        }
    }

    /// Put the part back in the state it powers up in.
    ///
    /// Only the command state; the contents are not touched, because a reset
    /// does not erase a flash chip. This matters more than it sounds: the
    /// operating system leaves the part in a status or query mode, and a
    /// processor reset that did not also reset the flash would fetch its
    /// first instruction from a chip answering `0xFFFFFFFF`. That is an
    /// undefined instruction, which vectors to `0x4`, where the next fetch is
    /// also `0xFFFFFFFF` -- a machine that resets straight into a tight loop
    /// on the undefined-instruction vector and looks like it has died.
    pub fn reset(&mut self) {
        self.mode = Mode::ReadArray;
    }

    pub fn read(&mut self, offset: u32, width: u32) -> u32 {
        match self.mode {
            Mode::ReadArray => self.read_array(offset, width),
            Mode::ReadStatus | Mode::ProgramSetup | Mode::EraseSetup | Mode::LockSetup => {
                Self::replicate(self.status)
            }
            Mode::ReadId => Self::replicate(self.id_word(offset >> 2)),
            Mode::ReadCfi => Self::replicate(self.cfi_word(offset >> 2)),
            // Mid buffered-write the device reports status.
            _ => Self::replicate(self.status),
        }
    }

    /// Read the array regardless of the current command mode. Used for
    /// instruction fetch: EBOOT deliberately probes the flash it is executing
    /// from, relying on the instruction cache to keep running.
    pub fn read_array_direct(&self, offset: u32, width: u32) -> u32 {
        self.read_array(offset, width)
    }

    fn read_array(&self, offset: u32, width: u32) -> u32 {
        let o = offset as usize;
        let n = width as usize;
        if o + n > self.data.len() {
            return 0xFFFF_FFFF;
        }
        let mut v = 0u32;
        for i in 0..n {
            v |= (self.data[o + i] as u32) << (8 * i);
        }
        v
    }

    pub fn write(&mut self, offset: u32, val: u32, width: u32) {
        // Both devices receive the same command in their own half of the bus,
        // so the low 16 bits carry it.
        let cmd = (val & 0xFFFF) as u16;
        self.commands_seen += 1;

        match self.mode {
            Mode::ProgramSetup => {
                self.program(offset, val, width);
                self.mode = Mode::ReadStatus;
                return;
            }
            Mode::EraseSetup => {
                if cmd == CMD_CONFIRM {
                    self.erase_block(offset);
                } else {
                    self.status |= SR_ERASE_ERROR;
                }
                self.mode = Mode::ReadStatus;
                return;
            }
            Mode::LockSetup => {
                let block = self.block_index(offset);
                match cmd {
                    0x01 => self.set_lock(block, true),
                    CMD_CONFIRM => self.set_lock(block, false),
                    _ => {}
                }
                self.mode = Mode::ReadStatus;
                return;
            }
            Mode::BufferCount => {
                // Count is words-minus-one, per device.
                let count = (cmd as u32 & 0xFF) + 1;
                self.mode = Mode::BufferData { addr: offset, remaining: count };
                return;
            }
            Mode::BufferData { addr, remaining } => {
                self.program(offset, val, width);
                let left = remaining.saturating_sub(1);
                if left == 0 {
                    self.mode = Mode::BufferConfirm;
                } else {
                    self.mode = Mode::BufferData { addr, remaining: left };
                }
                return;
            }
            Mode::BufferConfirm => {
                self.mode = Mode::ReadStatus;
                return;
            }
            _ => {}
        }

        match cmd {
            CMD_READ_ARRAY => self.mode = Mode::ReadArray,
            CMD_READ_ID => self.mode = Mode::ReadId,
            CMD_READ_CFI => self.mode = Mode::ReadCfi,
            CMD_READ_STATUS => self.mode = Mode::ReadStatus,
            CMD_CLEAR_STATUS => {
                self.status = SR_READY;
                self.mode = Mode::ReadArray;
            }
            CMD_PROGRAM | CMD_PROGRAM_ALT => self.mode = Mode::ProgramSetup,
            CMD_BLOCK_ERASE => self.mode = Mode::EraseSetup,
            CMD_LOCK_SETUP => self.mode = Mode::LockSetup,
            CMD_BUFFER_WRITE => self.mode = Mode::BufferCount,
            // Erase suspend and resume complete instantly here.
            0xB0 | CMD_CONFIRM => {}
            _ => self.mode = Mode::ReadArray,
        }
    }

    #[inline]
    fn block_index(&self, offset: u32) -> usize {
        (offset as usize / Self::bus_block_size()).min(self.locked.len().saturating_sub(1))
    }

    fn set_lock(&mut self, block: usize, locked: bool) {
        if let Some(b) = self.locked.get_mut(block) {
            *b = locked;
        }
    }

    /// Programming can only clear bits; setting one requires an erase.
    fn program(&mut self, offset: u32, val: u32, width: u32) {
        let block = self.block_index(offset);
        if self.locked.get(block).copied().unwrap_or(false) {
            self.status |= SR_PROGRAM_ERROR;
            return;
        }
        let o = offset as usize;
        let n = width as usize;
        if o + n > self.data.len() {
            self.status |= SR_PROGRAM_ERROR;
            return;
        }
        for i in 0..n {
            self.data[o + i] &= (val >> (8 * i)) as u8;
        }
        self.writes_performed += 1;
        self.status |= SR_READY;
    }

    fn erase_block(&mut self, offset: u32) {
        let block = self.block_index(offset);
        if self.locked.get(block).copied().unwrap_or(false) {
            self.status |= SR_ERASE_ERROR;
            return;
        }
        let size = Self::bus_block_size();
        let start = block * size;
        let end = (start + size).min(self.data.len());
        if start < end {
            self.data[start..end].fill(0xFF);
            self.writes_performed += 1;
        }
        self.status |= SR_READY;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// After the guest has used the part it is not in read-array mode, and a
    /// processor reset alone would fetch nonsense from it. Resetting the part
    /// is what makes the first instruction after a reset a real instruction.
    #[test]
    fn a_reset_puts_the_part_back_in_read_array_mode() {
        let mut f = NorFlash::new(1024);
        let instruction = f.read(0, 4);

        // Whatever the guest last asked it for -- a status read, an identify.
        f.write(0, CMD_READ_ID as u32, 2);
        assert_ne!(f.read(0, 4), instruction, "not the array any more");

        f.reset();
        assert_eq!(f.read(0, 4), instruction, "the array again, and unchanged");
    }

    fn flash() -> NorFlash {
        NorFlash::new(64 * 1024 * 1024)
    }

    #[test]
    fn reads_the_array_by_default() {
        let mut f = flash();
        f.data[0..4].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        assert_eq!(f.read(0, 4), 0x1234_5678);
    }

    #[test]
    fn cfi_query_returns_qry_replicated_across_both_devices() {
        let mut f = flash();
        f.write(0, 0x0098_0098, 4);
        // Device-word 0x10 is four bytes per word on this bus.
        assert_eq!(f.read(0x10 * 4, 4), 0x0051_0051, "Q");
        assert_eq!(f.read(0x11 * 4, 4), 0x0052_0052, "R");
        assert_eq!(f.read(0x12 * 4, 4), 0x0059_0059, "Y");
    }

    #[test]
    fn cfi_reports_a_geometry_that_covers_the_whole_bank() {
        let mut f = flash();
        f.write(0, 0x0098_0098, 4);
        let size_exp = f.read(0x27 * 4, 4) & 0xFFFF;
        let blocks = (f.read(0x2D * 4, 4) & 0xFF) | ((f.read(0x2E * 4, 4) & 0xFF) << 8);
        let units = (f.read(0x2F * 4, 4) & 0xFF) | ((f.read(0x30 * 4, 4) & 0xFF) << 8);
        let device_size = 1usize << size_exp;
        let block_size = units as usize * 256;
        assert_eq!(device_size, DEVICE_SIZE);
        assert_eq!((blocks as usize + 1) * block_size, DEVICE_SIZE);
        assert_eq!(device_size * INTERLEAVE as usize, f.data.len());
    }

    #[test]
    fn read_array_command_leaves_query_mode() {
        let mut f = flash();
        f.write(0, 0x0098_0098, 4);
        f.write(0, 0x00FF_00FF, 4);
        f.data[0x40..0x44].copy_from_slice(&0xAABB_CCDDu32.to_le_bytes());
        assert_eq!(f.read(0x40, 4), 0xAABB_CCDD);
    }

    #[test]
    fn identifier_mode_reports_intel() {
        let mut f = flash();
        f.write(0, 0x0090_0090, 4);
        assert_eq!(f.read(0, 4) & 0xFFFF, 0x0089);
        assert_eq!(f.read(4, 4) & 0xFFFF, 0x001D);
    }

    #[test]
    fn program_clears_bits_and_erase_restores_them() {
        let mut f = flash();
        f.write(0x1000, 0x0040_0040, 4); // program setup
        f.write(0x1000, 0x1234_5678, 4); // data

        // A program leaves the device reporting status, not the array.
        assert_eq!(f.read(0x1000, 4) & 0xFF, SR_READY as u32);
        f.write(0x1000, 0x00FF_00FF, 4);
        assert_eq!(f.read(0x1000, 4), 0x1234_5678);

        f.write(0x1000, 0x0020_0020, 4); // erase setup
        f.write(0x1000, 0x00D0_00D0, 4); // confirm
        f.write(0x1000, 0x00FF_00FF, 4); // back to array
        assert_eq!(f.read(0x1000, 4), 0xFFFF_FFFF);
    }

    #[test]
    fn erase_only_touches_its_own_block() {
        let mut f = flash();
        let next = NorFlash::bus_block_size() as u32;
        f.data[next as usize..next as usize + 4].copy_from_slice(&[0x11; 4]);
        f.write(0, 0x0020_0020, 4);
        f.write(0, 0x00D0_00D0, 4);
        f.write(0, 0x00FF_00FF, 4);
        assert_eq!(f.read(next, 4), 0x1111_1111, "neighbouring block untouched");
    }

    #[test]
    fn a_locked_block_refuses_to_erase() {
        let mut f = flash();
        f.data[0..4].copy_from_slice(&[0x00; 4]);
        f.write(0, 0x0060_0060, 4); // lock setup
        f.write(0, 0x0001_0001, 4); // lock
        f.write(0, 0x0020_0020, 4);
        f.write(0, 0x00D0_00D0, 4);
        assert_ne!(f.status & SR_ERASE_ERROR, 0);
        f.write(0, 0x00FF_00FF, 4);
        assert_eq!(f.read(0, 4), 0);
    }
}
