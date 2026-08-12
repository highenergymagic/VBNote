//! Intel PXA270 "Bulverde" on-chip peripherals.
//!
//! Only what the VoiceNote's firmware actually touches is modelled. Anything
//! else is recorded by the access log rather than silently returning zero, so
//! that bring-up failures show up as data instead of as a hang.

pub mod ac97;
pub mod dma;
pub mod gpio;
pub mod i2c;
pub mod intc;
pub mod mmc;
pub mod sdcard;
pub mod ohci;
pub mod ost;
pub mod power;
pub mod uart;

use ac97::Ac97;
use dma::Dma;
use gpio::Gpio;
use i2c::I2c;
use intc::Intc;
use mmc::Mmc;
use ost::Ost;
use power::{ClockManager, MemoryController, Pmu, Rtc};
use std::collections::BTreeMap;
use uart::Uart;

/// The PXA270's internal SRAM, 256 KB at 0x5C000000.
pub const SRAM_BASE: u32 = 0x5C00_0000;
pub const SRAM_SIZE: usize = 256 * 1024;

#[derive(Default, Clone, Copy)]
pub struct AccessStat {
    pub reads: u32,
    pub writes: u32,
    /// Program counter of the first access, which is what makes the log
    /// useful for finding the driver responsible.
    pub first_pc: u32,
    pub last_value: u32,
}

pub struct Pxa270 {
    pub intc: Intc,
    pub ac97: Ac97,
    pub dma: Dma,
    pub ost: Ost,
    pub mmc: Mmc,
    pub gpio: Gpio,
    pub i2c: I2c,
    pub ffuart: Uart,
    pub btuart: Uart,
    pub stuart: Uart,
    pub clocks: ClockManager,
    pub pmu: Pmu,
    pub rtc: Rtc,
    pub memc: MemoryController,
    /// The USB host controller, and the root hub a storage device plugs into.
    pub ohci: ohci::Ohci,
    pub sram: Vec<u8>,
    /// Accesses to peripherals we have not implemented.
    pub unimplemented: BTreeMap<u32, AccessStat>,
    /// Set by the runner before each instruction so the log can attribute
    /// unimplemented accesses to code.
    pub pc: u32,
    cpu_hz: u64,
}

impl Pxa270 {
    pub fn new(cpu_hz: u64) -> Self {
        Pxa270 {
            intc: Intc::default(),
            ac97: Ac97::default(),
            dma: Dma::default(),
            ost: Ost::new(cpu_hz),
            mmc: Mmc::default(),
            gpio: Gpio::default(),
            i2c: I2c::default(),
            ffuart: Uart::new(intc::IRQ_FFUART),
            btuart: Uart::new(intc::IRQ_BTUART),
            stuart: Uart::new(intc::IRQ_STUART),
            clocks: ClockManager::default(),
            pmu: Pmu::default(),
            rtc: Rtc::new(cpu_hz),
            memc: MemoryController::default(),
            ohci: ohci::Ohci::new(),
            sram: vec![0; SRAM_SIZE],
            unimplemented: BTreeMap::new(),
            pc: 0,
            cpu_hz,
        }
    }

    pub fn tick(&mut self, cycles: u32) {
        self.ost.tick(cycles, &mut self.intc);
        self.rtc.tick(cycles, &mut self.intc);
        self.ac97.tick(cycles, self.cpu_hz);
    }

    /// True when the physical address belongs to the SoC rather than the board.
    pub fn owns(&self, pa: u32) -> bool {
        matches!(pa & 0xFFF0_0000,
            0x4000_0000..=0x4200_0000 | 0x4400_0000 | 0x4800_0000 | 0x4C00_0000 | SRAM_BASE)
    }

    fn note(&mut self, pa: u32, write: bool, val: u32) {
        let e = self.unimplemented.entry(pa).or_insert(AccessStat {
            first_pc: self.pc,
            ..Default::default()
        });
        if write {
            e.writes += 1;
            e.last_value = val;
        } else {
            e.reads += 1;
        }
    }

    pub fn read32(&mut self, pa: u32) -> u32 {
        let base = pa & 0xFFF0_0000;
        let off = pa & 0x000F_FFFF;
        match base {
            uart::FFUART => self.ffuart.read(off, &mut self.intc),
            uart::BTUART => self.btuart.read(off, &mut self.intc),
            uart::STUART => self.stuart.read(off, &mut self.intc),
            ac97::BASE => self.ac97.read(off, &mut self.intc),
            dma::BASE => self.dma.read(off),
            ost::BASE => self.ost.read(off),
            mmc::BASE => self.mmc.read(off, &mut self.intc),
            intc::BASE => self.intc.read(off),
            gpio::BASE => self.gpio.read(off),
            i2c::BASE => self.i2c.read(off),
            power::CLKMGR_BASE => self.clocks.read(off),
            power::PMU_BASE => self.pmu.read(off),
            power::RTC_BASE => self.rtc.read(off),
            power::MEMC_BASE => self.memc.read(off),
            ohci::BASE => self.ohci.read(off, &mut self.intc),
            SRAM_BASE => {
                let o = off as usize & (SRAM_SIZE - 1);
                u32::from_le_bytes(self.sram[o..o + 4].try_into().unwrap())
            }
            _ => {
                self.note(pa, false, 0);
                0
            }
        }
    }

    pub fn write32(&mut self, pa: u32, val: u32) {
        let base = pa & 0xFFF0_0000;
        let off = pa & 0x000F_FFFF;
        match base {
            uart::FFUART => self.ffuart.write_from(off, val, &mut self.intc, self.pc),
            uart::BTUART => self.btuart.write_from(off, val, &mut self.intc, self.pc),
            uart::STUART => self.stuart.write_from(off, val, &mut self.intc, self.pc),
            ac97::BASE => self.ac97.write(off, val, &mut self.intc),
            dma::BASE => self.dma.write(off, val, &mut self.intc),
            ost::BASE => self.ost.write(off, val, &mut self.intc),
            mmc::BASE => self.mmc.write(off, val, &mut self.intc),
            intc::BASE => self.intc.write(off, val),
            gpio::BASE => {
                self.gpio.pc = self.pc;
                self.gpio.write(off, val, &mut self.intc)
            }
            i2c::BASE => self.i2c.write(off, val, &mut self.intc),
            power::CLKMGR_BASE => self.clocks.write(off, val),
            power::PMU_BASE => self.pmu.write(off, val),
            power::RTC_BASE => self.rtc.write(off, val, &mut self.intc),
            power::MEMC_BASE => self.memc.write(off, val),
            ohci::BASE => self.ohci.write(off, val, &mut self.intc),
            SRAM_BASE => {
                let o = off as usize & (SRAM_SIZE - 1);
                self.sram[o..o + 4].copy_from_slice(&val.to_le_bytes());
            }
            _ => self.note(pa, true, val),
        }
    }

    /// Byte and halfword accesses go through the word path. Every PXA
    /// register that firmware touches byte-wise is in the UARTs, where the
    /// low byte is the whole story.
    pub fn read8(&mut self, pa: u32) -> u8 {
        (self.read32(pa & !3) >> (8 * (pa & 3))) as u8
    }

    pub fn read16(&mut self, pa: u32) -> u16 {
        (self.read32(pa & !3) >> (8 * (pa & 2))) as u16
    }

    pub fn write8(&mut self, pa: u32, val: u8) {
        if pa & 0xFFF0_0000 == SRAM_BASE {
            self.sram[pa as usize & (SRAM_SIZE - 1)] = val;
            return;
        }
        self.write32(pa & !3, val as u32);
    }

    pub fn write16(&mut self, pa: u32, val: u16) {
        if pa & 0xFFF0_0000 == SRAM_BASE {
            let o = pa as usize & (SRAM_SIZE - 1);
            self.sram[o..o + 2].copy_from_slice(&val.to_le_bytes());
            return;
        }
        self.write32(pa & !3, val as u32);
    }

    #[inline]
    pub fn irq_pending(&self) -> bool {
        self.intc.irq_line()
    }

    #[inline]
    pub fn fiq_pending(&self) -> bool {
        self.intc.fiq_line()
    }
}
