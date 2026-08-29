//! Civil dates from epoch seconds.
//!
//! Replaces `chrono` and `time`. Rust's standard library can tell you that a
//! `SystemTime` is some number of seconds after 1970 and nothing else - there
//! is no calendar in `std` at all, so converting that number into a date people
//! recognise is the project's problem.
//!
//! The conversion is Howard Hinnant's `civil_from_days`, which handles the
//! proleptic Gregorian calendar without tables or loops. Dates before 1970 work
//! correctly, which matters because git repositories contain imported history
//! with timestamps from the 1980s and the occasional clock set wrong.
//!
//! No timezone database. Git records each commit's UTC offset alongside its
//! timestamp, so local time is recovered by adding the offset the committer's
//! machine reported. That is all the calendar strata needs.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

pub const SECONDS_PER_DAY: i64 = 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: i64,
    /// 1 to 12.
    pub month: u32,
    /// 1 to 31.
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl fmt::Display for DateTime {
    /// ISO 8601 date, which sorts correctly as text and is unambiguous
    /// everywhere. Reports never need the time of day.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl DateTime {
    pub fn iso_seconds(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Seconds since the Unix epoch, right now.
///
/// Before 1970 the system clock would have to be set absurdly; the saturating
/// conversion means such a clock yields zero rather than panicking.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Split epoch seconds into a civil date and a time of day.
pub fn from_epoch(seconds: i64) -> DateTime {
    // Rust's `/` and `%` truncate toward zero, but the calendar needs floor
    // division so that times before 1970 land on the right day.
    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let rest = seconds.rem_euclid(SECONDS_PER_DAY);

    let (year, month, day) = civil_from_days(days);

    DateTime {
        year,
        month,
        day,
        hour: (rest / 3600) as u32,
        minute: ((rest % 3600) / 60) as u32,
        second: (rest % 60) as u32,
    }
}

/// Days since 1970-01-01 to a calendar date, by Howard Hinnant's algorithm.
///
/// The trick is to shift the year so it starts in March, which puts the leap
/// day at the end and makes the month-length pattern regular enough to compute
/// rather than look up.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Re-base onto 0000-03-01, the start of a 400-year era.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097; // 0..=146096
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);

    // March is month 0 in this scheme; the 153/5 ratio is the average length of
    // a month in the repeating five-month pattern.
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;

    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Render a duration in whole days as something a person reads at a glance.
pub fn humanise_days(days: i64) -> String {
    match days {
        d if d < 0 => "in the future".to_string(),
        0 => "today".to_string(),
        1 => "1 day".to_string(),
        d if d < 60 => format!("{d} days"),
        d if d < 730 => format!("{} months", (d as f64 / 30.44).round() as i64),
        d => format!("{:.1} years", d as f64 / 365.25),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(seconds: i64) -> String {
        from_epoch(seconds).to_string()
    }

    #[test]
    fn converts_the_epoch_itself() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(from_epoch(0).hour, 0);
    }

    #[test]
    fn converts_known_timestamps() {
        assert_eq!(date(1_724_928_000), "2024-08-29");
        assert_eq!(date(1_000_000_000), "2001-09-09");
        assert_eq!(date(2_147_483_647), "2038-01-19");
    }

    #[test]
    fn handles_times_before_the_epoch() {
        // Truncating division would put these on the wrong day.
        assert_eq!(date(-1), "1969-12-31");
        assert_eq!(date(-86_400), "1969-12-31");
        assert_eq!(date(-86_401), "1969-12-30");
        assert_eq!(date(-2_208_988_800), "1900-01-01");
    }

    #[test]
    fn handles_leap_days() {
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(date(1_709_164_800), "2024-02-29");
        // 1900 was not a leap year, being divisible by 100 but not 400.
        assert_eq!(date(-2_203_977_600), "1900-02-28");
        assert_eq!(date(-2_203_891_200), "1900-03-01");
    }

    #[test]
    fn extracts_the_time_of_day() {
        // 1_724_889_600 is midnight UTC on 2024-08-29.
        let t = from_epoch(1_724_889_600 + 13 * 3600 + 45 * 60 + 7);
        assert_eq!(t.hour, 13);
        assert_eq!(t.minute, 45);
        assert_eq!(t.second, 7);
        assert_eq!(t.iso_seconds(), "2024-08-29T13:45:07Z");
    }

    #[test]
    fn keeps_the_time_of_day_for_a_mid_day_timestamp() {
        // The value from the brief's sample commit, which is not midnight.
        let t = from_epoch(1_724_928_000);
        assert_eq!(t.iso_seconds(), "2024-08-29T10:40:00Z");
    }

    #[test]
    fn time_of_day_is_right_before_the_epoch() {
        // One second before 1970 is 23:59:59 the day before, not -1 seconds.
        let t = from_epoch(-1);
        assert_eq!((t.hour, t.minute, t.second), (23, 59, 59));
    }

    #[test]
    fn round_trips_a_year_of_days() {
        // Walk a whole year one day at a time and check the date advances by
        // exactly one each step, which catches off-by-one errors in the era
        // arithmetic that spot checks would miss.
        let mut previous = from_epoch(0);
        for day in 1..=400i64 {
            let current = from_epoch(day * SECONDS_PER_DAY);
            let advanced = current.day == previous.day + 1
                || (current.day == 1 && current.month == previous.month + 1)
                || (current.day == 1 && current.month == 1 && current.year == previous.year + 1);
            assert!(advanced, "{previous:?} did not advance cleanly to {current:?}");
            previous = current;
        }
    }

    #[test]
    fn describes_durations() {
        assert_eq!(humanise_days(0), "today");
        assert_eq!(humanise_days(1), "1 day");
        assert_eq!(humanise_days(45), "45 days");
        assert_eq!(humanise_days(90), "3 months");
        assert_eq!(humanise_days(1000), "2.7 years");
    }
}
