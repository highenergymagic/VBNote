//! PXA27x GPIO, at physical 0x40E00000.
//!
//! 121 pins across four banks. EBOOT's first job after CPU setup is to drive
//! GPSR/GPCR across all four banks, so this has to be right early.

use crate::intc::{Intc, IRQ_GPIO0, IRQ_GPIO1, IRQ_GPIO_X};

pub const BASE: u32 = 0x40E0_0000;
pub const BANKS: usize = 4;

#[derive(Default)]
pub struct Gpio {
    /// Output latch, what the pin drives when configured as an output.
    pub out: [u32; BANKS],
    /// Direction: 1 is output.
    pub dir: [u32; BANKS],
    /// Level driven onto the pin from outside, for inputs.
    pub input: [u32; BANKS],
    pub rising: [u32; BANKS],
    pub falling: [u32; BANKS],
    /// Edge detect status, write-one-to-clear.
    pub edge: [u32; BANKS],
    /// Alternate function selects, two bits per pin.
    pub afr: [u32; BANKS * 2],
    /// Program counter of the write that first armed each pin's edge detect.
    ///
    /// A driver that waits on a pin instead of polling leaves no trace in
    /// access counts, but it has to arm the edge first, and that write says
    /// which module is waiting.
    pub armed_by: [[u32; 32]; BANKS],
    /// Set by the SoC before dispatching a write, so `armed_by` can be filled.
    pub pc: u32,
}

impl Gpio {
    /// Current level of every pin: outputs read back their latch, inputs read
    /// whatever the board is driving.
    #[inline]
    pub fn level(&self, bank: usize) -> u32 {
        (self.out[bank] & self.dir[bank]) | (self.input[bank] & !self.dir[bank])
    }

    /// Drive an input pin from the board, latching any enabled edge.
    pub fn set_input(&mut self, pin: u32, high: bool, intc: &mut Intc) {
        let (bank, bit) = ((pin / 32) as usize, 1u32 << (pin % 32));
        if bank >= BANKS {
            return;
        }
        let was = self.input[bank] & bit != 0;
        if high {
            self.input[bank] |= bit;
        } else {
            self.input[bank] &= !bit;
        }
        if !was && high && self.rising[bank] & bit != 0 {
            self.edge[bank] |= bit;
        }
        if was && !high && self.falling[bank] & bit != 0 {
            self.edge[bank] |= bit;
        }
        self.update_irq(intc);
    }

    /// GPIO 0 and 1 have dedicated interrupt lines; everything else shares one.
    pub fn update_irq(&self, intc: &mut Intc) {
        intc.set(IRQ_GPIO0, self.edge[0] & 1 != 0);
        intc.set(IRQ_GPIO1, self.edge[0] & 2 != 0);
        let shared = (self.edge[0] & !3) | self.edge[1] | self.edge[2] | self.edge[3];
        intc.set(IRQ_GPIO_X, shared != 0);
    }

    pub fn read(&self, offset: u32) -> u32 {
        // Banks 0-2 sit in a packed block; bank 3 was bolted on at 0x100.
        let (bank, reg) = match decode(offset) {
            Some(v) => v,
            None => return 0,
        };
        match reg {
            Reg::Level => self.level(bank),
            Reg::Dir => self.dir[bank],
            Reg::Rising => self.rising[bank],
            Reg::Falling => self.falling[bank],
            Reg::Edge => self.edge[bank],
            // GPSR and GPCR are write-only; the PXA returns the latch.
            Reg::Set | Reg::Clear => self.out[bank],
        }
    }

    pub fn write(&mut self, offset: u32, val: u32, intc: &mut Intc) {
        if (0x54..0x74).contains(&offset) {
            self.afr[((offset - 0x54) / 4) as usize] = val;
            return;
        }
        let (bank, reg) = match decode(offset) {
            Some(v) => v,
            None => return,
        };
        match reg {
            Reg::Dir => self.dir[bank] = val,
            Reg::Set => self.out[bank] |= val,
            Reg::Clear => self.out[bank] &= !val,
            Reg::Rising | Reg::Falling => {
                let newly = val & !(self.rising[bank] | self.falling[bank]);
                for bit in 0..32 {
                    if newly >> bit & 1 != 0 && self.armed_by[bank][bit] == 0 {
                        self.armed_by[bank][bit] = self.pc;
                    }
                }
                if reg == Reg::Rising {
                    self.rising[bank] = val;
                } else {
                    self.falling[bank] = val;
                }
            }
            Reg::Edge => self.edge[bank] &= !val,
            Reg::Level => {}
        }
        self.update_irq(intc);
    }
}

#[derive(Copy, Clone, PartialEq)]
enum Reg {
    Level,
    Dir,
    Set,
    Clear,
    Rising,
    Falling,
    Edge,
}

fn decode(offset: u32) -> Option<(usize, Reg)> {
    // Bank 3 lives at 0x100 with the same 0x0C spacing between register
    // groups as banks 0-2 have between themselves.
    if offset >= 0x100 {
        let reg = match offset {
            0x100 => Reg::Level,
            0x10C => Reg::Dir,
            0x118 => Reg::Set,
            0x124 => Reg::Clear,
            0x130 => Reg::Rising,
            0x13C => Reg::Falling,
            0x148 => Reg::Edge,
            _ => return None,
        };
        return Some((3, reg));
    }
    let group = offset / 0x0C;
    let bank = ((offset % 0x0C) / 4) as usize;
    let reg = match group {
        0 => Reg::Level,
        1 => Reg::Dir,
        2 => Reg::Set,
        3 => Reg::Clear,
        4 => Reg::Rising,
        5 => Reg::Falling,
        6 => Reg::Edge,
        _ => return None,
    };
    Some((bank, reg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_clear_drive_the_output_latch() {
        let mut gpio = Gpio::default();
        let mut intc = Intc::default();
        gpio.write(0x0C, 0xFFFF_FFFF, &mut intc); // GPDR0: all outputs
        gpio.write(0x18, 0x0000_00F0, &mut intc); // GPSR0
        assert_eq!(gpio.read(0x00) & 0xFF, 0xF0);
        gpio.write(0x24, 0x0000_0050, &mut intc); // GPCR0
        assert_eq!(gpio.read(0x00) & 0xFF, 0xA0);
    }

    #[test]
    fn bank_three_decodes_to_the_high_block() {
        assert_eq!(decode(0x118).map(|(b, r)| (b, r == Reg::Set)), Some((3, true)));
        assert_eq!(decode(0x20).map(|(b, r)| (b, r == Reg::Set)), Some((2, true)));
    }

    #[test]
    fn rising_edge_latches_and_raises_the_shared_interrupt() {
        let mut gpio = Gpio::default();
        let mut intc = Intc::default();
        intc.mask[0] = 1 << IRQ_GPIO_X;
        gpio.rising[1] = 1 << 5; // GPIO 37
        gpio.set_input(37, true, &mut intc);
        assert!(intc.irq_line());
        gpio.write(0x4C, 1 << 5, &mut intc); // clear GEDR1
        assert!(!intc.irq_line());
    }
}
