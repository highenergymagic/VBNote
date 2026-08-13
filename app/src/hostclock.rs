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
//! platform, and it is confined to this file: `GetLocalTime` on Windows,
//! `localtime_r` everywhere else. Neither is in `std`, because the zone is not
//! a constant the program can hold -- it is whatever the host's rules say for
//! this instant, summer and winter included -- and asking the platform is
//! cheaper than a dependency that carries the zoneinfo database.
//!
//! The arithmetic that turns a date into a count of seconds is in
//! [`pxa270::power`], where it can be tested without a clock at all.

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

#[cfg(unix)]
mod platform {
    use std::os::raw::{c_int, c_long};

    /// `struct tm`, as `localtime_r` fills it in.
    ///
    /// POSIX fixes the order of the first nine fields, and those nine are all
    /// this reads. The tail is not portable -- glibc and the BSDs carry
    /// `tm_gmtoff` and `tm_zone` after them, others do not -- so the struct
    /// carries room for those two and never looks at either. Declaring a tail
    /// that is too *short* would be a buffer the C library writes past;
    /// declaring one that is merely unread costs nothing.
    #[repr(C)]
    struct Tm {
        sec: c_int,
        min: c_int,
        hour: c_int,
        mday: c_int,
        /// 0 to 11, unlike every other field here.
        mon: c_int,
        /// Years since 1900.
        year: c_int,
        wday: c_int,
        yday: c_int,
        isdst: c_int,
        gmtoff: c_long,
        zone: *const u8,
    }

    extern "C" {
        fn localtime_r(time: *const i64, result: *mut Tm) -> *mut Tm;
    }

    /// The host's local wall clock, through the C library, which is the only
    /// thing that knows the zone: the offset is not a constant, it is whatever
    /// `TZ` and the zoneinfo database say for *this* instant, so a summer
    /// reading and a winter one differ. `std` will not do this, and the
    /// alternative is a dependency to answer one question.
    ///
    /// `time_t` is taken as 64-bit, which it is everywhere this is built.
    /// Should it ever be built somewhere with a 32-bit one, a little-endian
    /// host reads the low half and stays right until 2038; a big-endian one
    /// would need this widened.
    pub fn local_now() -> Option<u32> {
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        // Safe: `localtime_r` writes only the struct it is handed, which is
        // big enough and lives for the call, and reads only a plain integer.
        // It is the reentrant one precisely so that no static is shared.
        let mut tm: Tm = unsafe { std::mem::zeroed() };
        if unsafe { localtime_r(&unix, &mut tm) }.is_null() {
            return None;
        }
        Some(pxa270::power::rtc_count_for(
            tm.year as i64 + 1900,
            tm.mon as i64 + 1,
            tm.mday as i64,
            tm.hour as i64,
            tm.min as i64,
            tm.sec as i64,
        ))
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    /// UTC, for a platform with neither of the calls above. The machine is
    /// then wrong by the timezone rather than by sixteen years, and can still
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

    /// Local and UTC differ by a timezone offset and nothing else.
    ///
    /// This is what a mis-declared `struct tm` would fail. Reading the fields
    /// at the wrong offsets still yields a date, and a date still lands inside
    /// the range the test above checks, so that one would pass while the
    /// machine announced the wrong day. A real offset is between -12 and +14
    /// hours; anything outside a day is a decoding fault, not a zone.
    ///
    /// It cannot assert that the two are *unequal*, because on a host set to
    /// UTC they are equal and correctly so.
    #[test]
    fn the_clock_is_local_and_differs_from_utc_only_by_a_zone() {
        let local = super::now().expect("the host should know the time") as i64;
        let utc = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the host clock should be after 1970")
            .as_secs()
            - pxa270::power::EPOCH_IN_UNIX_TIME) as i64;
        let offset = local - utc;
        assert!(
            offset.abs() <= 24 * 3600,
            "local time is {offset} s from UTC, which is no timezone"
        );
    }
}
