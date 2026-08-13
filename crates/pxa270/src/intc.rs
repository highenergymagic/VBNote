//! PXA27x interrupt controller, at physical 0x40D00000.
//!
//! Two banks of 32 sources. Each can be routed to IRQ or FIQ by ICLR.

pub const BASE: u32 = 0x40D0_0000;

// Interrupt source numbers used by the drivers we care about.
/// The two USB host interrupts. Measured rather than assumed: `ohci.dll`
/// calls `RequestSysIntr` twice on a boot, with 3 and then 2, which is the
/// machine's two USB host ports.
pub const IRQ_USB_HOST_2: u32 = 2;
pub const IRQ_USB_HOST_1: u32 = 3;
pub const IRQ_OST_4_11: u32 = 7;
pub const IRQ_GPIO0: u32 = 8;
pub const IRQ_GPIO1: u32 = 9;
pub const IRQ_GPIO_X: u32 = 10;
pub const IRQ_PMU: u32 = 12;
pub const IRQ_AC97: u32 = 14;
pub const IRQ_I2C: u32 = 18;
pub const IRQ_STUART: u32 = 20;
pub const IRQ_BTUART: u32 = 21;
pub const IRQ_FFUART: u32 = 22;
pub const IRQ_MMC: u32 = 23;
pub const IRQ_DMA: u32 = 25;
pub const IRQ_OST0: u32 = 26;
pub const IRQ_RTC_HZ: u32 = 30;
pub const IRQ_RTC_ALARM: u32 = 31;

pub struct Intc {
    /// Raw level of every source, banks 0 and 1 packed into one word each.
    pub pending: [u32; 2],
    pub mask: [u32; 2],
    /// 1 routes the source to FIQ instead of IRQ.
    pub level: [u32; 2],
    pub control: u32,
    /// Priority registers. Stored so reads read back, otherwise unused.
    priority: [u32; 40],
}

impl Default for Intc {
    fn default() -> Self {
        Intc {
            pending: [0; 2],
            mask: [0; 2],
            level: [0; 2],
            control: 0,
            priority: [0; 40],
        }
    }
}

impl Intc {
    /// Set or clear an interrupt source. Sources are level-triggered.
    pub fn set(&mut self, source: u32, active: bool) {
        let (bank, bit) = ((source / 32) as usize, source % 32);
        if bank > 1 {
            return;
        }
        if active {
            self.pending[bank] |= 1 << bit;
        } else {
            self.pending[bank] &= !(1 << bit);
        }
    }

    /// The sources the guest has put in its priority table, in order.
    ///
    /// Worth being able to see: if the table is programmed and a source is
    /// missing from it, `ichp` will not report that source, the OAL will not
    /// recognise the interrupt, and the driver waiting on it is never
    /// signalled -- which looks like a slow device rather than a lost wake.
    pub fn programmed_sources(&self) -> Vec<u32> {
        self.priority
            .iter()
            .filter(|p| *p & IPR_VALID != 0)
            .map(|p| p & 0x3F)
            .collect()
    }

    /// Whether the guest has this source masked off.
    ///
    /// A device that raises a masked line has not interrupted anybody: the
    /// driver will find out some other way, on some other schedule, and the
    /// difference between the two is latency nobody accounts for.
    pub fn is_masked(&self, source: u32) -> bool {
        let (bank, bit) = ((source / 32) as usize, source % 32);
        bank < 2 && self.mask[bank] & (1 << bit) == 0
    }

    /// Whether a source is asserted, regardless of whether it is masked.
    /// For a device to check its own line without reaching into the bank.
    pub fn is_pending(&self, source: u32) -> bool {
        let (bank, bit) = ((source / 32) as usize, source % 32);
        bank < 2 && self.pending[bank] & (1 << bit) != 0
    }

    #[inline]
    pub fn irq_line(&self) -> bool {
        (0..2).any(|b| self.pending[b] & self.mask[b] & !self.level[b] != 0)
    }

    #[inline]
    pub fn fiq_line(&self) -> bool {
        (0..2).any(|b| self.pending[b] & self.mask[b] & self.level[b] != 0)
    }

    /// Is this source asserted, unmasked, and routed to the requested line?
    #[inline]
    fn active(&self, source: u32, want_fiq: bool) -> bool {
        let (bank, bit) = ((source / 32) as usize, 1u32 << (source % 32));
        if bank > 1 {
            return false;
        }
        let asserted = self.pending[bank] & self.mask[bank] & bit != 0;
        let is_fiq = self.level[bank] & bit != 0;
        asserted && is_fiq == want_fiq
    }

    /// The source ICHP should report, honouring the priority table when the
    /// guest has programmed one.
    fn highest_priority(&self, want_fiq: bool) -> Option<u32> {
        let mut any_valid = false;
        for p in self.priority.iter() {
            if p & IPR_VALID == 0 {
                continue;
            }
            any_valid = true;
            let source = p & 0x3F;
            if self.active(source, want_fiq) {
                return Some(source);
            }
        }
        if any_valid {
            // A programmed table is authoritative: a source missing from it
            // is not reported.
            None
        } else {
            (0..64).find(|s| self.active(*s, want_fiq))
        }
    }

    /// ICHP, the highest-priority pending interrupt.
    ///
    /// Windows CE's OAL reads this rather than scanning ICIP, so without it
    /// every interrupt resolves to source 0 and the kernel logs
    /// `In ISRUnknown IRQ:0` forever.
    /// With nothing pending the source fields read `0x3f`, not zero: `0x3f`
    /// is the "no such source" sentinel, and zero is the OS timer's
    /// neighbour. A reader that trusts the field without checking the valid
    /// bit gets told "source 63", which is nothing, rather than "source 0",
    /// which is a real interrupt.
    pub fn ichp(&self) -> u32 {
        let mut v = ICHP_IDLE;
        if let Some(irq) = self.highest_priority(false) {
            v &= 0x0000_FFFF;
            v |= ICHP_VAL_IRQ | (irq << 16);
        }
        if let Some(fiq) = self.highest_priority(true) {
            v &= 0xFFFF_0000;
            v |= ICHP_VAL_FIQ | fiq;
        }
        v
    }

    /// Map a register offset to a priority-table index. IPR0-31 are packed
    /// from 0x1C, then the second bank's control registers intervene and
    /// IPR32-39 resume at 0xB0.
    fn priority_index(offset: u32) -> Option<usize> {
        match offset {
            0x1C..=0x98 => Some(((offset - 0x1C) / 4) as usize),
            0xB0..=0xCC => Some(32 + ((offset - 0xB0) / 4) as usize),
            _ => None,
        }
    }

    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            0x00 => self.pending[0] & self.mask[0] & !self.level[0], // ICIP
            0x04 => self.mask[0],                                    // ICMR
            0x08 => self.level[0],                                   // ICLR
            0x0C => self.pending[0] & self.mask[0] & self.level[0],  // ICFP
            0x10 => self.pending[0],                                 // ICPR
            0x14 => self.control,                                    // ICCR
            0x18 => self.ichp(),                                     // ICHP
            0x9C => self.pending[1] & self.mask[1] & !self.level[1], // ICIP2
            0xA0 => self.mask[1],                                    // ICMR2
            0xA4 => self.level[1],                                   // ICLR2
            0xA8 => self.pending[1] & self.mask[1] & self.level[1],  // ICFP2
            0xAC => self.pending[1],                                 // ICPR2
            _ => Self::priority_index(offset)
                .and_then(|i| self.priority.get(i).copied())
                .unwrap_or(0),
        }
    }

    pub fn write(&mut self, offset: u32, val: u32) {
        match offset {
            0x04 => self.mask[0] = val,
            0x08 => self.level[0] = val,
            0x14 => self.control = val,
            0xA0 => self.mask[1] = val,
            0xA4 => self.level[1] = val,
            // ICIP, ICFP, ICPR and ICHP are read-only: sources are
            // level-triggered and clear when the device is serviced.
            _ => {
                if let Some(i) = Self::priority_index(offset) {
                    if let Some(p) = self.priority.get_mut(i) {
                        *p = val;
                    }
                }
            }
        }
    }
}

/// ICHP bit 31: an IRQ is pending, and bits 30:16 name it.
pub const ICHP_VAL_IRQ: u32 = 1 << 31;
/// ICHP bit 15: an FIQ is pending, and bits 14:0 name it.
pub const ICHP_VAL_FIQ: u32 = 1 << 15;
/// Both source fields set to the invalid id, which is what the register
/// reads when nothing is pending.
pub const ICHP_IDLE: u32 = 0x003F_003F;
/// IPR entry bit 31 marks the entry as programmed.
pub const IPR_VALID: u32 = 1 << 31;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ichp_reports_nothing_when_idle() {
        let intc = Intc::default();
        assert_eq!(intc.ichp(), ICHP_IDLE, "both source ids invalid");
    }

    #[test]
    fn ichp_names_the_pending_source() {
        let mut intc = Intc::default();
        intc.mask[0] = 1 << IRQ_OST0;
        intc.set(IRQ_OST0, true);
        let ichp = intc.ichp();
        assert_ne!(ichp & ICHP_VAL_IRQ, 0, "an IRQ is pending");
        assert_eq!((ichp >> 16) & 0x7FFF, IRQ_OST0, "and it is the OS timer");
    }

    #[test]
    fn a_masked_source_is_not_reported() {
        let mut intc = Intc::default();
        intc.set(IRQ_OST0, true);
        assert_eq!(intc.ichp(), ICHP_IDLE, "masked sources stay invisible");
    }

    #[test]
    fn fiq_routing_uses_the_low_half() {
        let mut intc = Intc::default();
        intc.mask[0] = 1 << IRQ_FFUART;
        intc.level[0] = 1 << IRQ_FFUART;
        intc.set(IRQ_FFUART, true);
        let ichp = intc.ichp();
        assert_eq!(ichp & ICHP_VAL_IRQ, 0, "not an IRQ");
        assert_ne!(ichp & ICHP_VAL_FIQ, 0, "an FIQ");
        assert_eq!(ichp & 0x7FFF, IRQ_FFUART);
    }

    #[test]
    fn a_programmed_priority_table_decides_the_order() {
        let mut intc = Intc::default();
        intc.mask[0] = (1 << IRQ_OST0) | (1 << IRQ_FFUART);
        intc.set(IRQ_OST0, true);
        intc.set(IRQ_FFUART, true);
        // Without a table, the lowest source number wins.
        assert_eq!((intc.ichp() >> 16) & 0x7FFF, IRQ_FFUART);
        // Programme the timer as top priority and it takes over.
        intc.write(0x1C, IPR_VALID | IRQ_OST0);
        assert_eq!((intc.ichp() >> 16) & 0x7FFF, IRQ_OST0);
    }

    #[test]
    fn priority_registers_map_across_both_blocks() {
        assert_eq!(Intc::priority_index(0x1C), Some(0));
        assert_eq!(Intc::priority_index(0x98), Some(31));
        assert_eq!(Intc::priority_index(0xB0), Some(32));
        assert_eq!(Intc::priority_index(0xCC), Some(39));
        // The second bank's control registers must not be mistaken for IPRs.
        assert_eq!(Intc::priority_index(0xA0), None);
        assert_eq!(Intc::priority_index(0xAC), None);
    }

    #[test]
    fn ichp_is_read_only() {
        let mut intc = Intc::default();
        intc.mask[0] = 1 << IRQ_OST0;
        intc.set(IRQ_OST0, true);
        intc.write(0x18, 0);
        assert_ne!(intc.ichp() & ICHP_VAL_IRQ, 0);
    }
}
