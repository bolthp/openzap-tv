//! Local wall-clock time for showing programme times.
//!
//! std has no time zones, so the offset comes from the operating system
//! (`localtime_r` on Unix, `SystemTimeToTzSpecificLocalTime` on Windows),
//! which also knows the daylight-saving rules.

/// Offset of local time from UTC at Unix time `unix`, in seconds
/// (e.g. +10800 for Moscow). 0 if the system cannot tell.
pub fn utc_offset(unix: i64) -> i64 {
    sys::utc_offset(unix).unwrap_or(0)
}

/// "HH:MM" in local time.
pub fn local_hm(unix: i64) -> String {
    format_hm(unix, utc_offset(unix))
}

/// "HH:MM" for Unix time `unix` shifted by `offset` seconds.
pub fn format_hm(unix: i64, offset: i64) -> String {
    let secs = (unix + offset).rem_euclid(86_400);
    format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// Days since 1970-01-01 → (year, month 1–12, day 1–31). Inverse of
/// `days_from_civil` (Howard Hinnant's algorithm).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// (year, month, day) → days since 1970-01-01.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let (m, d) = (i64::from(m), i64::from(d));
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(all(unix, target_pointer_width = "64"))]
mod sys {
    use std::ffi::{c_char, c_int, c_long};

    /// `struct tm` as in glibc, musl and macOS (all have `tm_gmtoff`).
    #[repr(C)]
    struct Tm {
        tm_sec: c_int,
        tm_min: c_int,
        tm_hour: c_int,
        tm_mday: c_int,
        tm_mon: c_int,
        tm_year: c_int,
        tm_wday: c_int,
        tm_yday: c_int,
        tm_isdst: c_int,
        tm_gmtoff: c_long,
        tm_zone: *const c_char,
    }

    unsafe extern "C" {
        // `time_t` is 64-bit on the 64-bit targets this module is built for.
        fn localtime_r(time: *const i64, result: *mut Tm) -> *mut Tm;
    }

    pub fn utc_offset(unix: i64) -> Option<i64> {
        let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
        // SAFETY: both pointers are valid for the call; localtime_r is the
        // thread-safe variant and fills `tm` when it returns non-NULL.
        let result = unsafe { localtime_r(&unix, tm.as_mut_ptr()) };
        if result.is_null() {
            return None;
        }
        // SAFETY: initialized by the successful call above.
        Some(unsafe { tm.assume_init() }.tm_gmtoff)
    }
}

#[cfg(windows)]
mod sys {
    use std::ffi::c_void;

    use super::{civil_from_days, days_from_civil};

    /// `SYSTEMTIME`.
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

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SystemTimeToTzSpecificLocalTime(
            zone: *const c_void,
            utc: *const SystemTime,
            local: *mut SystemTime,
        ) -> i32;
    }

    pub fn utc_offset(unix: i64) -> Option<i64> {
        let (year, month, day) = civil_from_days(unix.div_euclid(86_400));
        let secs = unix.rem_euclid(86_400);
        let utc = SystemTime {
            year: u16::try_from(year).ok()?,
            month: month as u16,
            day: day as u16,
            hour: (secs / 3600) as u16,
            minute: (secs % 3600 / 60) as u16,
            second: (secs % 60) as u16,
            ..SystemTime::default()
        };
        let mut local = SystemTime::default();
        // SAFETY: valid pointers; NULL zone means the current time zone.
        let ok = unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) };
        if ok == 0 {
            return None;
        }
        let local_secs = days_from_civil(
            i64::from(local.year),
            u32::from(local.month),
            u32::from(local.day),
        ) * 86_400
            + i64::from(local.hour) * 3600
            + i64::from(local.minute) * 60
            + i64::from(local.second);
        Some(local_secs - unix)
    }
}

#[cfg(not(any(windows, all(unix, target_pointer_width = "64"))))]
mod sys {
    pub fn utc_offset(_unix: i64) -> Option<i64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_hours_and_minutes() {
        // 2026-11-02 18:05:59 UTC
        let t = 1_793_642_759;
        assert_eq!(format_hm(t, 0), "18:05");
        assert_eq!(format_hm(t, 3 * 3600), "21:05");
        // Crossing midnight in both directions.
        assert_eq!(format_hm(t, 6 * 3600), "00:05");
        assert_eq!(format_hm(t, -19 * 3600), "23:05");
    }

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(20_759), (2026, 11, 2));
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        for days in (-800_000..800_000).step_by(997) {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn system_offset_is_plausible() {
        // Real offsets lie within UTC-12..UTC+14 and are whole quarter hours.
        let offset = utc_offset(1_793_642_759);
        assert!((-12 * 3600..=14 * 3600).contains(&offset), "{offset}");
        assert_eq!(offset % 900, 0, "{offset}");
    }
}
