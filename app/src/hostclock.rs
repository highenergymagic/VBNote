//! What time it is, in the terms the machine's clock counts in.
//!
//! A real mPower keeps time with a backup cell, so it knows what time it is
//! the moment it is switched on. This emulator's RTC started from zero every
//! run, which the machine reports as midnight on 1 January 2010 and then asks
//! the user to correct -- three questions, out loud, at every single boot, to
//! somebody who cannot see the screen. For a machine whose whole point is that
//! it needs no setting up, that is a fault rather than a missing feature.
//!
//! So the emulator hands it the host's clock instead, which is what the cell
//! would have been doing.
//!
//! # Local time, not UTC
//!
//! Windows CE's OAL hands the kernel *local* time and lets `GetSystemTime`
//! take the bias back off, so what belongs in `RCNR` is the local wall clock.
//! Seeding it with UTC would leave the machine wrong by the offset -- right
//! for anyone on Greenwich, and quietly wrong for everybody else.
//!
//! Reading a local wall clock is the one thing here that has to ask the
//! platform, and it is confined to this file. The arithmetic that turns a date
//! into a count of seconds is in [`pxa270::power`], where it can be tested
//! without a clock at all.

/// Seconds since the machine's epoch, right now, by the host's local clock.
///
/// `None` if the host will not say, in which case the machine starts at its
/// own epoch exactly as it used to and asks.
pub fn now() -> Option<u32> {
    platform::local_now()
}

#[cfg(windows)]
mod platform {
    /// `SYSTEMTIME`, as `GetLocalTime` fills it in.
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    extern "system" {
        fn GetLocalTime(out: *mut SystemTime);
    }

    pub fn local_now() -> Option<u32> {
        let mut t = SystemTime::default();
        // Safe: the call only writes the struct it is given, which is the
        // right size and lives for the call.
        unsafe { GetLocalTime(&mut t) };
        if t.year == 0 {
            return None;
        }
        Some(pxa270::power::rtc_count_for(
            t.year as i64,
            t.month as i64,
            t.day as i64,
            t.hour as i64,
            t.minute as i64,
            t.second as i64,
        ))
    }
}

#[cfg(not(windows))]
mod platform {
    /// UTC, because finding the local offset portably means a dependency and
    /// this is not the platform the machine is used on. The clock will be out
    /// by the timezone rather than by sixteen years, and the machine can still
    /// be told.
    pub fn local_now() -> Option<u32> {
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        unix.checked_sub(pxa270::power::EPOCH_IN_UNIX_TIME)
            .map(|s| s.min(u32::MAX as u64) as u32)
    }
}

#[cfg(test)]
mod tests {
    /// Whatever the host says, it has to be a time this machine could be
    /// switched on at: after its epoch, and not centuries hence. A wrong
    /// answer here is a plausible-looking date, so the test is a range rather
    /// than a value.
    #[test]
    fn the_host_clock_lands_in_this_machines_lifetime() {
        let now = super::now().expect("the host should know the time");
        let year_2020 = pxa270::power::rtc_count_for(2020, 1, 1, 0, 0, 0);
        let year_2200 = pxa270::power::rtc_count_for(2200, 1, 1, 0, 0, 0);
        assert!(now > year_2020, "the clock is stuck in the past: {now}");
        assert!(now < year_2200, "the clock is in the far future: {now}");
    }
}
