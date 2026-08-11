//! The "Gandalf" board: a PXA270 plus the parts HumanWare added around it.
//!
//! Physical memory map, taken from the OAL's OEMAddressTable (see
//! docs/hardware.md):
//!
//! ```text
//!   0x00000000  64 MB  nCS0  boot NOR flash, KeySoft system image, XIP
//!   0x10000000   1 MB  nCS4  board CPLD
//!   0x40000000  32 MB        PXA270 internal peripherals
//!   0x5C000000 256 KB        PXA270 internal SRAM
//!   0xA0000000  64 MB        SDRAM
//! ```

pub mod braille;
pub mod cpld;
pub mod dma;
pub mod flash;
pub mod keyboard;
pub mod licence;
pub mod modem;
pub mod nkp;
pub mod onewire;
pub mod patch;
pub mod power;
pub mod provision;
pub mod registry;

use arm::Bus;
use cpld::Cpld;
use flash::NorFlash;
use power::PowerState;
use pxa270::{AccessStat, Pxa270};
use std::collections::BTreeMap;

pub const FLASH_BASE: u32 = 0x0000_0000;
pub const FLASH_SIZE: usize = 64 * 1024 * 1024;
pub const CPLD_BASE: u32 = 0x1000_0000;
/// The two chip selects the DiskOnChip sockets sit on. Nothing is fitted to
/// them, and nothing models them — but they have to *read* like an empty
/// socket, which is all ones rather than all zeroes. A bus with no device
/// driving it floats high.
///
/// This matters: `trueffs.dll` probes here and goes on probing. Zeroes are a
/// plausible answer from a chip that is present and unwell, so it keeps
/// trying; ones are what absence looks like.
pub const DOC_NCS1_BASE: u32 = 0x0400_0000;
pub const DOC_NCS3_BASE: u32 = 0x0C00_0000;
pub const SDRAM_BASE: u32 = 0xA000_0000;
pub const SDRAM_SIZE: usize = 64 * 1024 * 1024;

/// Nominal core clock. The PXA270 in this machine runs at 312 MHz; the exact
/// figure only affects how guest time maps to host time.
pub const CPU_HZ: u64 = 312_000_000;

/// A larger figure to run the guest's clocks against.
///
/// The interpreter charges a few cycles for every instruction and models no
/// caches, so it retires far less work per emulated second than a real
/// PXA270. Everything the guest times against the wall clock therefore fires
/// early — including the power manager's idle timeout, which suspends the
/// machine mid-boot. Telling the timers the core is faster than it claims
/// stretches guest time relative to instructions executed, which is the
/// closest single knob to "the CPU keeps up with the real one".
///
/// This is a stopgap. The real fix is a faster core, at which point the
/// figure comes back down to `CPU_HZ`.
pub const CPU_HZ_EFFECTIVE: u64 = 1_200_000_000;

/// What to clock the guest at by default.
///
/// Not the real machine's figure and not the effective one: what *this*
/// interpreter can actually retire in a second. At `CPU_HZ_EFFECTIVE` the
/// emulator manages about **6%** of real time, and since the guest produces
/// its audio in guest time, six percent of real time means six percent of the
/// sound and a device draining the rest as silence -- a machine that stutters
/// so badly it sounds broken. At this figure it holds 100%.
///
/// It is a floor rather than an ideal: a faster host could run the guest
/// harder, and `--cpu-mhz` is there for that. Measured not to let the power
/// manager's idle timeout suspend the machine mid-boot, which is what a
/// figure chosen carelessly does.
pub const CPU_HZ_DEFAULT: u64 = 63_000_000;

pub struct Gandalf {
    pub soc: Pxa270,
    pub sdram: Vec<u8>,
    pub flash: NorFlash,
    pub cpld: Cpld,
    pub power: PowerState,
    power_divider: u32,
    /// The EEPROM holding the machine's serial number, on GPIO 22.
    pub onewire: onewire::OneWire,
    /// Emulated cycles since start.
    pub elapsed: u64,
    /// Accesses that hit nothing at all.
    pub unmapped: BTreeMap<u32, AccessStat>,
    /// Updated by the runner before each instruction, for attribution.
    pub pc: u32,
}

impl Gandalf {
    pub fn new() -> Self {
        Gandalf::with_clock(CPU_HZ_EFFECTIVE)
    }

    /// Build a board whose timers run against `cpu_hz`.
    pub fn with_clock(cpu_hz: u64) -> Self {
        let mut board = Gandalf {
            soc: Pxa270::new(cpu_hz),
            sdram: vec![0; SDRAM_SIZE],
            flash: NorFlash::new(FLASH_SIZE),
            cpld: Cpld::default(),
            power: PowerState::default(),
            power_divider: 0,
            onewire: onewire::OneWire::default(),
            elapsed: 0,
            unmapped: BTreeMap::new(),
            pc: 0,
        };
        // Present the sense pins before the guest runs, so the very first
        // read the battery driver makes already sees mains power.
        board.power.drive(&mut board.soc.gpio, &mut board.soc.intc);
        board
    }

    /// Copy a Windows CE image's records into memory at their target
    /// addresses, translating kernel virtual addresses to physical ones.
    ///
    /// CE images are linked for the static kernel mapping, so a record aimed
    /// at 0x96C79000 belongs at physical 0xA0079000.
    pub fn load_image(&mut self, image: &ceromfs::CeImage) -> Result<u32, String> {
        for r in &image.records {
            let pa = kernel_va_to_pa(r.addr)
                .ok_or_else(|| format!("record at {:#010x} is outside the static map", r.addr))?;
            self.write_block(pa, &r.data)?;
        }
        kernel_va_to_pa(image.launch)
            .ok_or_else(|| format!("launch address {:#010x} is outside the static map", image.launch))
    }

    /// Place a bootloader image into NOR flash the way the real board stores
    /// it, and return the reset vector.
    ///
    /// The image's base address corresponds to flash offset zero: EBOOT's
    /// first record is the four-byte reset branch, and its own flash-to-SDRAM
    /// copy loop reads from physical 0 and writes to the physical address its
    /// base maps to. Loading it anywhere else makes that copy overwrite the
    /// running code with erased-flash bytes.
    pub fn load_flash_image(&mut self, image: &ceromfs::CeImage) -> Result<u32, String> {
        for r in &image.records {
            let off = r.addr.wrapping_sub(image.base) as usize;
            if off + r.data.len() > FLASH_SIZE {
                return Err(format!(
                    "record at {:#010x} maps to flash offset {:#x}, past the end of the device",
                    r.addr, off
                ));
            }
            self.flash.data[off..off + r.data.len()].copy_from_slice(&r.data);
        }
        Ok(0)
    }

    /// Install a complete, already-laid-out flash image.
    pub fn load_raw_flash(&mut self, image: &[u8]) -> Result<(), String> {
        if image.len() > FLASH_SIZE {
            return Err(format!(
                "flash image is {} MB, larger than the {} MB device",
                image.len() / (1024 * 1024),
                FLASH_SIZE / (1024 * 1024)
            ));
        }
        self.flash.data[..image.len()].copy_from_slice(image);
        self.flash.data[image.len()..].fill(0xFF);
        Ok(())
    }

    fn write_block(&mut self, pa: u32, data: &[u8]) -> Result<(), String> {
        let (mem, off) = if (SDRAM_BASE..SDRAM_BASE + SDRAM_SIZE as u32).contains(&pa) {
            (&mut self.sdram, (pa - SDRAM_BASE) as usize)
        } else if (pa as usize) < FLASH_SIZE {
            (&mut self.flash.data, pa as usize)
        } else {
            return Err(format!("no memory at {pa:#010x}"));
        };
        if off + data.len() > mem.len() {
            return Err(format!("block at {pa:#010x} runs past the end of memory"));
        }
        mem[off..off + data.len()].copy_from_slice(data);
        Ok(())
    }

    /// A CPLD write, plus the one board wire that depends on it.
    ///
    /// The braille display's far end is on GPIO 103, and the OAL reads that
    /// pin between bytes to find out how long the chain is. So the pin has to
    /// follow the shift register on every write, not on a poll.
    fn cpld_write(&mut self, offset: u32, val: u16) {
        self.cpld.write(offset, val, self.pc);
        if offset == cpld::BRAILLE_REG {
            let level = self.cpld.braille.end_of_chain();
            self.soc.gpio.set_input(braille::END_OF_CHAIN_GPIO, level, &mut self.soc.intc);
        }
        if modem::Modem::owns(offset) {
            self.drive_modem_interrupt();
        }
    }

    /// A CPLD read, plus the interrupt line that a read can lower.
    ///
    /// Reading `IIR` is how the driver acknowledges the modem, and reading the
    /// data register is what empties it, so both change whether the part is
    /// still asking to be served.
    fn cpld_read(&mut self, offset: u32) -> u16 {
        let val = self.cpld.read(offset, self.pc);
        if modem::Modem::owns(offset) {
            self.drive_modem_interrupt();
        }
        val
    }

    /// Let the modem put its interrupt line where it belongs.
    ///
    /// The pin is armed for a rising edge, so the line has to fall again
    /// between interrupts or the second one is never noticed. Driving it from
    /// the part's own state on every register access does that by itself: the
    /// driver's read of `IIR` is what lowers it.
    fn drive_modem_interrupt(&mut self) {
        let level = self.cpld.modem.interrupting();
        self.soc.gpio.set_input(modem::INTERRUPT_GPIO, level, &mut self.soc.intc);
    }

    /// Put the board back the way it powers up, without disturbing anything
    /// that survives a reset.
    ///
    /// Memory keeps its contents and so does the flash array, exactly as on
    /// the real machine -- the bootloader is in flash and does not need
    /// putting back. What has to go is the state the running system left in
    /// the *devices*: a flash chip mid-command answers a fetch with
    /// `0xFFFFFFFF`, and the keyboard would come back with keys still held.
    pub fn reset(&mut self) {
        self.flash.reset();
        self.cpld.keyboard.release_all();
    }

    /// Let the 1-Wire device see the pin, and report back what it drives.
    ///
    /// The bus is open drain: the master pulls low by driving the pin as an
    /// output with a zero in the latch, and lets go by turning it back into
    /// an input. So "the master is pulling" is exactly that pair of bits.
    fn drive_onewire(&mut self) {
        let pin = onewire::PIN;
        let (bank, bit) = ((pin / 32) as usize, 1u32 << (pin % 32));
        let g = &self.soc.gpio;
        let master_low = g.dir[bank] & bit != 0 && g.out[bank] & bit == 0;
        // Timed by OSCR, the same counter the kernel's delay loop spins on.
        let now = self.soc.ost.oscr;
        let level = self.onewire.update(master_low, now);
        self.soc.gpio.set_input(pin, level, &mut self.soc.intc);
    }

    fn note_unmapped(&mut self, pa: u32, write: bool, val: u32) {
        let e = self
            .unmapped
            .entry(pa)
            .or_insert(AccessStat { first_pc: self.pc, ..Default::default() });
        if write {
            e.writes += 1;
            e.last_value = val;
        } else {
            e.reads += 1;
        }
    }

    #[inline]
    fn sdram_off(pa: u32) -> Option<usize> {
        let off = pa.wrapping_sub(SDRAM_BASE) as usize;
        (off < SDRAM_SIZE).then_some(off)
    }
}

impl Default for Gandalf {
    fn default() -> Self {
        Gandalf::new()
    }
}

/// Map a Windows CE static kernel virtual address to a physical one.
///
/// Only the two entries the boot path needs are handled; the peripheral
/// windows are already physical in every image we load.
pub fn kernel_va_to_pa(va: u32) -> Option<u32> {
    match va {
        0x9660_0000..=0x9AC0_0000 => Some(va - 0x96C0_0000 + SDRAM_BASE),
        0x8000_0000..=0x83FF_FFFF => Some(va - 0x8000_0000),
        // Already physical.
        0x0000_0000..=0x5FFF_FFFF | 0xA000_0000..=0xA3FF_FFFF => Some(va),
        _ => None,
    }
}

macro_rules! dispatch_read {
    ($self:ident, $pa:expr, $width:ty, $from_le:path) => {{
        let pa = $pa;
        if let Some(off) = Gandalf::sdram_off(pa) {
            let n = std::mem::size_of::<$width>();
            return $from_le($self.sdram[off..off + n].try_into().unwrap());
        }
        if (pa as usize) < FLASH_SIZE {
            let n = std::mem::size_of::<$width>() as u32;
            return $self.flash.read(pa, n) as $width;
        }
        0
    }};
}

impl Bus for Gandalf {
    fn read8(&mut self, pa: u32) -> u8 {
        match pa & 0xFFF0_0000 {
            CPLD_BASE => self.cpld_read(pa & 0xFFFFF) as u8,
            DOC_NCS1_BASE | DOC_NCS3_BASE => 0xFF,
            b if is_soc(b) => self.soc.read8(pa),
            _ => dispatch_read!(self, pa, u8, u8::from_le_bytes),
        }
    }

    fn read16(&mut self, pa: u32) -> u16 {
        match pa & 0xFFF0_0000 {
            CPLD_BASE => self.cpld_read(pa & 0xFFFFF),
            DOC_NCS1_BASE | DOC_NCS3_BASE => 0xFFFF,
            b if is_soc(b) => self.soc.read16(pa),
            _ => dispatch_read!(self, pa, u16, u16::from_le_bytes),
        }
    }

    fn read32(&mut self, pa: u32) -> u32 {
        match pa & 0xFFF0_0000 {
            CPLD_BASE => self.cpld_read(pa & 0xFFFFF) as u32,
            DOC_NCS1_BASE | DOC_NCS3_BASE => 0xFFFF_FFFF,
            b if is_soc(b) => self.soc.read32(pa),
            _ => {
                if let Some(off) = Gandalf::sdram_off(pa) {
                    return u32::from_le_bytes(self.sdram[off..off + 4].try_into().unwrap());
                }
                if (pa as usize) + 4 <= FLASH_SIZE {
                    return self.flash.read(pa, 4);
                }
                self.note_unmapped(pa, false, 0);
                0
            }
        }
    }

    fn write8(&mut self, pa: u32, val: u8) {
        match pa & 0xFFF0_0000 {
            CPLD_BASE => self.cpld_write(pa & 0xFFFFF, val as u16),
            b if is_soc(b) => self.soc.write8(pa, val),
            _ => {
                if let Some(off) = Gandalf::sdram_off(pa) {
                    self.sdram[off] = val;
                } else if (pa as usize) < FLASH_SIZE {
                    self.flash.write(pa, val as u32, 1);
                } else {
                    self.note_unmapped(pa, true, val as u32);
                }
            }
        }
    }

    fn write16(&mut self, pa: u32, val: u16) {
        match pa & 0xFFF0_0000 {
            CPLD_BASE => self.cpld_write(pa & 0xFFFFF, val),
            b if is_soc(b) => self.soc.write16(pa, val),
            _ => {
                if let Some(off) = Gandalf::sdram_off(pa) {
                    self.sdram[off..off + 2].copy_from_slice(&val.to_le_bytes());
                } else if (pa as usize) < FLASH_SIZE {
                    self.flash.write(pa, val as u32, 2);
                } else {
                    self.note_unmapped(pa, true, val as u32);
                }
            }
        }
    }

    fn write32(&mut self, pa: u32, val: u32) {
        match pa & 0xFFF0_0000 {
            CPLD_BASE => self.cpld_write(pa & 0xFFFFF, val as u16),
            b if is_soc(b) => {
                self.soc.write32(pa, val);
                // The serial number's EEPROM hangs off a GPIO, so it has to
                // see the pin move on the write that moves it.
                if b == pxa270::gpio::BASE & 0xFFF0_0000 {
                    self.drive_onewire();
                }
            }
            _ => {
                if let Some(off) = Gandalf::sdram_off(pa) {
                    self.sdram[off..off + 4].copy_from_slice(&val.to_le_bytes());
                } else if (pa as usize) < FLASH_SIZE {
                    self.flash.write(pa, val, 4);
                } else {
                    self.note_unmapped(pa, true, val);
                }
            }
        }
    }

    fn fetch32(&mut self, pa: u32) -> u32 {
        if let Some(off) = Gandalf::sdram_off(pa) {
            return u32::from_le_bytes(self.sdram[off..off + 4].try_into().unwrap());
        }
        if (pa as usize) + 4 <= FLASH_SIZE {
            return self.flash.read_array_direct(pa, 4);
        }
        self.read32(pa)
    }

    fn fetch16(&mut self, pa: u32) -> u16 {
        if let Some(off) = Gandalf::sdram_off(pa) {
            return u16::from_le_bytes(self.sdram[off..off + 2].try_into().unwrap());
        }
        if (pa as usize) + 2 <= FLASH_SIZE {
            return self.flash.read_array_direct(pa, 2) as u16;
        }
        self.read16(pa)
    }

    fn tick(&mut self, cycles: u32) {
        self.elapsed = self.elapsed.wrapping_add(cycles as u64);
        self.soc.tick(cycles);
        // The device answers by holding the line down for a while, so the pin
        // has to be refreshed as time passes and not only when it is written.
        self.drive_onewire();
        // Give the DMA engine a chance to advance, once for every cycle in the
        // batch rather than once per call.
        //
        // One per call was right when this was called every instruction. Once
        // the runner started batching, it silently became one descriptor per
        // batch — transfers ran a hundred times slower in guest time, and the
        // SD driver, which moves the flash disk's data this way, waited for a
        // write that was never going to finish in the time it allowed. It hung
        // on the first setting KeySoft tried to save.
        let mut budget = cycles;
        while budget > 0 && self.soc.dma.next_runnable().is_some() {
            if !crate::dma::service(self) {
                break;
            }
            budget -= 1;
        }
        // The charge line is sampled repeatedly to detect toggling, so it has
        // to be refreshed rather than set once.
        self.power_divider = self.power_divider.wrapping_add(cycles);
        if self.power_divider > 100_000 {
            self.power_divider = 0;
            let mut power = self.power;
            power.drive(&mut self.soc.gpio, &mut self.soc.intc);
            self.power = power;
        }
    }

    fn irq_pending(&self) -> bool {
        self.soc.irq_pending()
    }

    fn fiq_pending(&self) -> bool {
        self.soc.fiq_pending()
    }
}

/// The PXA270 scatters its blocks across 0x40000000-0x5C000000 rather than
/// packing them, so this is a list rather than one range.
#[inline]
fn is_soc(base: u32) -> bool {
    matches!(base,
        0x4000_0000..=0x4200_0000  // peripheral bus: DMA through keypad
        | 0x4400_0000              // LCD controller
        | 0x4800_0000              // static memory controller
        | 0x4C00_0000              // USB host OHCI
        | 0x5C00_0000              // internal SRAM
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_addresses_map_to_physical() {
        assert_eq!(kernel_va_to_pa(0x96C7_9000), Some(0xA007_9000));
        assert_eq!(kernel_va_to_pa(0x8004_1000), Some(0x0004_1000));
        assert_eq!(kernel_va_to_pa(0xA000_0000), Some(0xA000_0000));
        assert_eq!(kernel_va_to_pa(0xC000_0000), None);
    }

    #[test]
    fn sdram_round_trips() {
        let mut b = Gandalf::new();
        b.write32(SDRAM_BASE + 0x1000, 0xDEAD_BEEF);
        assert_eq!(b.read32(SDRAM_BASE + 0x1000), 0xDEAD_BEEF);
        assert_eq!(b.read8(SDRAM_BASE + 0x1000), 0xEF);
        assert_eq!(b.read16(SDRAM_BASE + 0x1002), 0xDEAD);
    }

    #[test]
    fn erased_flash_reads_as_ones() {
        let mut b = Gandalf::new();
        assert_eq!(b.read32(0x1000), 0xFFFF_FFFF);
    }

    #[test]
    fn flash_answers_a_cfi_query_through_the_bus() {
        let mut b = Gandalf::new();
        b.write32(0, 0x0098_0098);
        assert_eq!(b.read32(0x10 * 4), 0x0051_0051);
        b.write32(0, 0x00FF_00FF);
    }

    #[test]
    fn uart_writes_reach_the_soc() {
        let mut b = Gandalf::new();
        b.write32(pxa270::uart::FFUART, b'Z' as u32);
        assert_eq!(b.soc.ffuart.drain_tx(), b"Z");
    }
}
