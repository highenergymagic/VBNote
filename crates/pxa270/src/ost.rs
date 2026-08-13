//! PXA27x OS timers, at physical 0x40A00000.
//!
//! OSCR counts at 3.25 MHz. Windows CE's OAL uses match register 0 for the
//! system tick and reads OSCR for its high-resolution counter, so this is the
//! one peripheral whose rate has to be right or the guest's sense of time
//! drifts.

use crate::intc::{Intc, IRQ_OST0};

pub const BASE: u32 = 0x40A0_0000;
/// 3.25 MHz, which is the PXA27x rate and **not** the PXA25x's 3.6864 MHz.
///
/// This was 3.6864 MHz, and that is the older part: the PXA25x runs its OS
/// timer at 3.6864 MHz, the PXA27x at 3.25 MHz -- 308 ns a tick, from the
/// 13 MHz oscillator divided by four. Three sources agree and one of them is
/// this machine's own firmware: `sdmmc.dll`'s `StallExecution` at
/// `0x03dc49b8` multiplies its argument by **3250** before spinning on OSCR,
/// which is only a millisecond at 3.25 MHz.
///
/// Getting it wrong ran every timed thing in the guest 13.4% fast: the
/// system tick fired early, so `GetTickCount` gained time, and every
/// `StallExecution` in every driver came back short.
pub const OST_HZ: u64 = 3_250_000;

pub struct Ost {
    pub osmr: [u32; 4],
    pub oscr: u32,
    /// Match status; the guest clears bits by writing 1.
    pub ossr: u32,
    pub oier: u32,
    pub ower: u32,
    /// Fractional cycle accumulator, in CPU cycles scaled by OST_HZ.
    frac: u64,
    cpu_hz: u64,
}

impl Ost {
    pub fn new(cpu_hz: u64) -> Self {
        Ost {
            osmr: [0; 4],
            oscr: 0,
            ossr: 0,
            oier: 0,
            ower: 0,
            frac: 0,
            cpu_hz,
        }
    }

    pub fn tick(&mut self, cycles: u32, intc: &mut Intc) {
        self.frac += cycles as u64 * OST_HZ;
        let ticks = self.frac / self.cpu_hz;
        if ticks == 0 {
            return;
        }
        self.frac -= ticks * self.cpu_hz;

        let old = self.oscr;
        let new = old.wrapping_add(ticks as u32);
        self.oscr = new;

        for ch in 0..4 {
            if crossed(old, new, self.osmr[ch]) {
                self.ossr |= 1 << ch;
            }
        }
        self.update_irq(intc);
    }

    fn update_irq(&self, intc: &mut Intc) {
        for ch in 0..4 {
            let active = self.oier & (1 << ch) != 0 && self.ossr & (1 << ch) != 0;
            intc.set(IRQ_OST0 + ch as u32, active);
        }
    }

    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            0x00..=0x0C => self.osmr[(offset / 4) as usize],
            0x10 => self.oscr,
            0x14 => self.ossr,
            0x18 => self.ower,
            0x1C => self.oier,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u32, val: u32, intc: &mut Intc) {
        match offset {
            0x00..=0x0C => self.osmr[(offset / 4) as usize] = val,
            0x10 => {
                self.oscr = val;
                self.frac = 0;
            }
            // Write-one-to-clear.
            0x14 => self.ossr &= !val,
            0x18 => self.ower = val,
            0x1C => self.oier = val,
            _ => {}
        }
        self.update_irq(intc);
    }
}

/// Did the counter pass `target` while going from `old` to `new`?
/// Handles the 32-bit wrap.
#[inline]
fn crossed(old: u32, new: u32, target: u32) -> bool {
    if old <= new {
        target > old && target <= new
    } else {
        target > old || target <= new
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_advances_at_the_timer_rate() {
        let cpu_hz = 520_000_000u64;
        let mut ost = Ost::new(cpu_hz);
        let mut intc = Intc::default();
        // One second of CPU cycles, fed in realistic slices.
        let slice = 1000u32;
        for _ in 0..(cpu_hz / slice as u64) {
            ost.tick(slice, &mut intc);
        }
        let drift = (ost.oscr as i64 - OST_HZ as i64).abs();
        assert!(drift < 10, "OSCR was {} after a second", ost.oscr);
    }

    #[test]
    fn match_raises_and_clears_its_interrupt() {
        // A CPU clock equal to the timer's gives one OSCR tick per cycle,
        // whatever the timer's rate is.
        let mut ost = Ost::new(OST_HZ);
        let mut intc = Intc::default();
        ost.write(0x00, 100, &mut intc); // OSMR0
        ost.write(0x1C, 1, &mut intc); // OIER channel 0
        intc.mask[0] = 1 << IRQ_OST0;

        ost.tick(50, &mut intc);
        assert!(!intc.irq_line());
        ost.tick(60, &mut intc);
        assert!(intc.irq_line(), "match should have fired");

        ost.write(0x14, 1, &mut intc); // clear OSSR bit 0
        assert!(!intc.irq_line());
    }

    /// The rate is the PXA27x's, not the PXA25x's, and this pins it.
    ///
    /// Three sources agree: the PXA27x manual gives a 308 ns period, QEMU's
    /// `pxa27x-timer` defaults to 3,250,000 where its `pxa25x-timer` defaults
    /// to 3,686,400, and this machine's own `sdmmc.dll` multiplies a
    /// millisecond by 3250 before spinning on OSCR. The older rate is 13.4%
    /// fast, which shortens every driver delay and gains the guest time.
    #[test]
    fn the_rate_is_the_pxa27x_one() {
        assert_eq!(OST_HZ, 3_250_000, "3.6864 MHz is the PXA25x");
        assert_eq!(1_000_000_000 / OST_HZ, 307, "308 ns a tick, near enough");
    }

    #[test]
    fn crossing_handles_wraparound() {
        assert!(crossed(0xFFFF_FFF0, 0x0000_0010, 0));
        assert!(crossed(0xFFFF_FFF0, 0x0000_0010, 0xFFFF_FFFF));
        assert!(!crossed(0xFFFF_FFF0, 0x0000_0010, 0x8000_0000));
    }
}
