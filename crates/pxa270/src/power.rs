//! Clock manager, power manager and real-time clock.
//!
//! EBOOT reads all three during startup: it prints the run-mode and turbo
//! multipliers from CCCR, and it reads RCSR to decide whether it came up from
//! a cold boot, a watchdog reset or a resume from sleep.

use crate::intc::{Intc, IRQ_RTC_ALARM, IRQ_RTC_HZ};

pub const CLKMGR_BASE: u32 = 0x4130_0000;
pub const PMU_BASE: u32 = 0x40F0_0000;
pub const RTC_BASE: u32 = 0x4090_0000;

pub struct ClockManager {
    pub cccr: u32,
    pub cken: u32,
    pub oscc: u32,
    pub ccsr: u32,
}

impl Default for ClockManager {
    fn default() -> Self {
        // L = 16, 2N = 2 (turbo x1), A = 0: a 13 MHz crystal times L gives a
        // 208 MHz run mode, which is a configuration the PXA270 ships in and
        // which EBOOT prints without complaint.
        let cccr = 16 | (2 << 7);
        ClockManager { cccr, cken: 0, oscc: 0, ccsr: cccr }
    }
}

impl ClockManager {
    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            0x00 => self.cccr,
            0x04 => self.cken,
            0x08 => self.oscc,
            0x0C => self.ccsr,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u32, val: u32) {
        match offset {
            0x00 => {
                self.cccr = val;
                self.ccsr = val;
            }
            0x04 => self.cken = val,
            0x08 => {
                // Bit 1 requests the 32.768 kHz oscillator; bit 0 reports it
                // running. Firmware spins on bit 0, so grant it immediately.
                self.oscc = val | ((val & 0x2) >> 1);
            }
            _ => {}
        }
    }
}

/// Reset controller status bits, reported in RCSR.
///
/// Bit 2 is **sleep-mode reset**, not a software one -- the names are from the
/// PXA27x manual and Linux agrees (`RCSR_SMR`). It is how the firmware tells a
/// resume from a cold boot: waking this part is architecturally a reset, and
/// the only thing that says the memory is still worth anything is this bit.
pub const RCSR_HARDWARE: u32 = 1 << 0;
pub const RCSR_WATCHDOG: u32 = 1 << 1;
pub const RCSR_SLEEP: u32 = 1 << 2;
pub const RCSR_GPIO: u32 = 1 << 3;

pub struct Pmu {
    regs: [u32; 64],
    pub rcsr: u32,
}

impl Default for Pmu {
    fn default() -> Self {
        Pmu { regs: [0; 64], rcsr: RCSR_HARDWARE }
    }
}

impl Pmu {
    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            0x30 => self.rcsr,
            _ => self.regs.get((offset / 4) as usize).copied().unwrap_or(0),
        }
    }

    pub fn write(&mut self, offset: u32, val: u32) {
        match offset {
            // RCSR is write-one-to-clear.
            0x30 => self.rcsr &= !val,
            _ => {
                if let Some(r) = self.regs.get_mut((offset / 4) as usize) {
                    *r = val;
                }
            }
        }
    }
}

/// Seconds from the Unix epoch to the one this machine counts from.
///
/// `RCNR` counts seconds from midnight on **1 January 1980**. Read out of the
/// OAL rather than guessed: the routine at `0x800820f4` takes the counter,
/// divides it down into seconds, minutes and hours, and then walks a year
/// loop that starts at `0x7bc`, which is 1980. Its day-of-week is
/// `(days + 2) mod 7` with Sunday zero, making day zero a Tuesday -- and
/// 1 January 1980 was a Tuesday, which is the second, independent check that
/// the base is right.
///
/// Worth knowing why this was got wrong once: the machine announces midnight
/// on 1 January **2010** when it has no clock, so 2010 looks like the epoch.
/// It is not; it is what KeySoft offers when it has decided the clock needs
/// setting. The two differ by 10958 days -- a whole number of days -- so
/// seeding with the wrong one gives exactly the right time of day and a date
/// thirty years out, which is a hard mistake to see.
pub const EPOCH_IN_UNIX_TIME: u64 = 315_532_800;

/// Days from 1970-01-01 to a civil date, by Howard Hinnant's algorithm.
///
/// Written out rather than pulled in, because one function is not worth a
/// dependency and a wrong answer here would show up as a plausible date on a
/// machine nobody can check against a calendar.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// What `RCNR` should hold for a given wall-clock time.
///
/// Saturates rather than wrapping: a date before the machine's epoch has no
/// representation, and counting backwards from zero would put the machine
/// somewhere in 2146.
pub fn rtc_count_for(y: i64, mo: i64, d: i64, h: i64, mi: i64, sec: i64) -> u32 {
    let days = days_from_civil(y, mo, d) - days_from_civil(1980, 1, 1);
    let total = days * 86_400 + h * 3_600 + mi * 60 + sec;
    total.clamp(0, u32::MAX as i64) as u32
}

pub struct Rtc {
    pub rcnr: u32,
    pub rtar: u32,
    pub rtsr: u32,
    pub rttr: u32,
    frac: u64,
    cpu_hz: u64,
    /// Reads and writes per register, by offset / 4.
    ///
    /// Seeding the clock is only worth anything if the guest looks at it, and
    /// "the machine still asks for the time" does not say whether it read a
    /// register and disliked the answer or never read it at all. Per register
    /// rather than in total, because this part has two sets: the seconds
    /// counter, and the day and year registers above it that decide the date.
    pub accesses: [(u64, u64); 16],
}

// RTSR bits.
const RTSR_AL: u32 = 1 << 0;
const RTSR_HZ: u32 = 1 << 1;
const RTSR_ALE: u32 = 1 << 2;
const RTSR_HZE: u32 = 1 << 3;

impl Rtc {
    pub fn new(cpu_hz: u64) -> Self {
Rtc {
            rcnr: 0,
            rtar: 0,
            rtsr: 0,
            rttr: 0x7FFF,
            frac: 0,
            cpu_hz,
            accesses: [(0, 0); 16],
        }
    }

    /// Start the clock at a given count of seconds since the machine's epoch.
    ///
    /// A real one is kept going by a backup cell, so it knows the time when it
    /// is switched on. This one starts at zero unless it is told, which is why
    /// the machine used to ask a blind user to set the clock at every single
    /// boot.
    pub fn set_count(&mut self, seconds: u32) {
        self.rcnr = seconds;
        self.frac = 0;
    }

    pub fn tick(&mut self, cycles: u32, intc: &mut Intc) {
        self.frac += cycles as u64;
        if self.frac < self.cpu_hz {
            return;
        }
        let seconds = self.frac / self.cpu_hz;
        self.frac -= seconds * self.cpu_hz;
        let before = self.rcnr;
        self.rcnr = self.rcnr.wrapping_add(seconds as u32);
        self.rtsr |= RTSR_HZ;
        if before < self.rtar && self.rcnr >= self.rtar {
            self.rtsr |= RTSR_AL;
        }
        self.update_irq(intc);
    }

    fn update_irq(&self, intc: &mut Intc) {
        intc.set(IRQ_RTC_HZ, self.rtsr & RTSR_HZE != 0 && self.rtsr & RTSR_HZ != 0);
        intc.set(IRQ_RTC_ALARM, self.rtsr & RTSR_ALE != 0 && self.rtsr & RTSR_AL != 0);
    }

    pub fn read(&mut self, offset: u32) -> u32 {
        if let Some(a) = self.accesses.get_mut(offset as usize / 4) {
            a.0 += 1;
        }
        match offset {
            0x00 => self.rcnr,
            0x04 => self.rtar,
            0x08 => self.rtsr,
            0x0C => self.rttr,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u32, val: u32, intc: &mut Intc) {
        if let Some(a) = self.accesses.get_mut(offset as usize / 4) {
            a.1 += 1;
        }
        match offset {
            0x00 => self.rcnr = val,
            0x04 => {
                self.rtar = val;
                self.rtsr &= !RTSR_AL;
            }
            // The enable bits are read/write; the status bits below them are
            // write-one-to-clear.
            0x08 => {
                let enables = val & (RTSR_ALE | RTSR_HZE);
                let clears = val & (RTSR_AL | RTSR_HZ);
                self.rtsr = (self.rtsr & !(RTSR_ALE | RTSR_HZE) & !clears) | enables;
            }
            0x0C => self.rttr = val,
            _ => {}
        }
        self.update_irq(intc);
    }
}

/// Static memory controller. The values only matter to EBOOT's own reporting,
/// so this is a register file with plausible reset values.
pub struct MemoryController {
    regs: [u32; 128],
}

pub const MEMC_BASE: u32 = 0x4800_0000;

impl Default for MemoryController {
    fn default() -> Self {
        let mut regs = [0u32; 128];
        regs[0] = 0x0300_0AC9; // MDCNFG: two banks of SDRAM enabled
        regs[1] = 0x0003_00A3; // MDREFR
        regs[2] = 0x7FF0_7FF0; // MSC0, covering nCS0/nCS1
        regs[3] = 0x7FF0_7FF0; // MSC1, covering nCS2/nCS3
        regs[4] = 0x7FF0_7FF0; // MSC2, covering nCS4/nCS5
        MemoryController { regs }
    }
}

impl MemoryController {
    pub fn read(&self, offset: u32) -> u32 {
        self.regs.get((offset / 4) as usize).copied().unwrap_or(0)
    }
    pub fn write(&mut self, offset: u32, val: u32) {
        if let Some(r) = self.regs.get_mut((offset / 4) as usize) {
            *r = val;
        }
    }
}

#[cfg(test)]
mod rtc_epoch_tests {
    use super::*;

    /// The counter is zero at the machine's own epoch, which the OAL says is
    /// 1980 and not the 2010 the machine announces when it has no clock.
    #[test]
    fn the_epoch_itself_is_zero() {
        assert_eq!(rtc_count_for(1980, 1, 1, 0, 0, 0), 0);
    }

    #[test]
    fn a_day_and_a_year_are_the_right_length() {
        assert_eq!(rtc_count_for(1980, 1, 2, 0, 0, 0), 86_400);
        assert_eq!(rtc_count_for(1981, 1, 1, 0, 0, 0), 366 * 86_400, "1980 was a leap year");
        assert_eq!(rtc_count_for(1980, 1, 1, 1, 2, 3), 3_600 + 120 + 3);
    }

    /// The two epochs differ by a whole number of days, which is exactly why
    /// using the wrong one produced a convincing time and a date thirty years
    /// out. Pinned so that mistake cannot come back quietly.
    #[test]
    fn the_date_the_machine_offers_when_it_has_no_clock_is_not_the_epoch() {
        let announced = rtc_count_for(2010, 1, 1, 0, 0, 0);
        assert_ne!(announced, 0);
        assert_eq!(announced % 86_400, 0, "a whole number of days apart");
        assert_eq!(announced / 86_400, 10_958, "thirty years, eight of them leap");
    }

    /// 2012 is a leap year and 2100 is not, which is the case a naive
    /// every-fourth-year rule gets wrong.
    #[test]
    fn leap_years_are_counted_properly() {
        let day = 86_400;
        assert_eq!(
            rtc_count_for(2013, 1, 1, 0, 0, 0) - rtc_count_for(2012, 1, 1, 0, 0, 0),
            366 * day
        );
        assert_eq!(
            rtc_count_for(2101, 1, 1, 0, 0, 0) - rtc_count_for(2100, 1, 1, 0, 0, 0),
            365 * day
        );
    }

    /// It agrees with Unix time, which is the other clock in the room.
    #[test]
    fn it_agrees_with_unix_time() {
        let unix_2020 = 1_577_836_800u64; // 2020-01-01T00:00:00Z
        assert_eq!(
            rtc_count_for(2020, 1, 1, 0, 0, 0) as u64,
            unix_2020 - EPOCH_IN_UNIX_TIME
        );
    }

    /// A date the machine cannot represent clamps rather than wrapping round
    /// to a confident and completely wrong answer.
    #[test]
    fn dates_before_the_epoch_clamp_to_it() {
        assert_eq!(rtc_count_for(1979, 12, 31, 23, 59, 59), 0);
    }

    /// The day of the week has to come out right too, because the OAL derives
    /// it from the same count: `(days + 2) mod 7`, Sunday being zero. A
    /// Thursday that reads as a Wednesday means the epoch is a day out.
    #[test]
    fn the_day_of_the_week_follows_from_the_count() {
        let dow = |y, m, d| (rtc_count_for(y, m, d, 0, 0, 0) / 86_400 + 2) % 7;
        assert_eq!(dow(1980, 1, 1), 2, "a Tuesday");
        assert_eq!(dow(2010, 1, 1), 5, "a Friday");
        assert_eq!(dow(2000, 2, 29), 2, "a Tuesday, and a leap day");
    }

    /// Setting the clock is what a backup cell does for a real one.
    #[test]
    fn the_count_can_be_set() {
        let mut rtc = Rtc::new(1000);
        assert_eq!(rtc.rcnr, 0);
        rtc.set_count(rtc_count_for(2026, 8, 10, 14, 30, 0));
        assert_ne!(rtc.rcnr, 0);
        let count = rtc.rcnr;
        assert_eq!(rtc.read(0x00), count, "and the guest can read it back");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oscillator_reports_ready_once_requested() {
        let mut c = ClockManager::default();
        assert_eq!(c.read(0x08) & 1, 0);
        c.write(0x08, 0x2);
        assert_eq!(c.read(0x08) & 1, 1, "firmware spins on OOK");
    }

    #[test]
    fn rcsr_reports_cold_boot_and_clears_on_write() {
        let mut p = Pmu::default();
        assert_eq!(p.read(0x30), RCSR_HARDWARE);
        p.write(0x30, RCSR_HARDWARE);
        assert_eq!(p.read(0x30), 0);
    }

    #[test]
    fn rtc_counts_seconds() {
        let mut rtc = Rtc::new(1000);
        let mut intc = Intc::default();
        for _ in 0..3 {
            rtc.tick(1000, &mut intc);
        }
        assert_eq!(rtc.rcnr, 3);
    }
}
