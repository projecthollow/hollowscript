//! Proleptic Gregorian calendar arithmetic with no dependency, for the
//! `clock` capability's date and time formatting (sections 15.2 and 16.3).
//!
//! `days_from_civil` and `civil_from_days` are Howard Hinnant's well known
//! constant-time algorithms (public domain), which agree with the
//! specification's date range and are exact for every year from 0001 to
//! 9999 and well beyond it in both directions.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "day and month are always in 1..=31 and 1..=12 by the algorithm's own domain"
)]

/// The number of days since 1970-01-01 for a Gregorian calendar date.
#[must_use]
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (i64::from(month) + 9) % 12; // Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// The Gregorian calendar date (year, month, day) of a day count since
/// 1970-01-01.
#[must_use]
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day)
}

/// The weekday of a day count, 1 for Monday to 7 for Sunday (section 15.2).
/// 1970-01-01 was a Thursday.
#[must_use]
pub(crate) fn weekday_from_days(z: i64) -> u32 {
    let w = (z + 3).rem_euclid(7); // 0 = Monday
    u32::try_from(w).unwrap_or(0) + 1
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, days_from_civil, weekday_from_days};

    #[test]
    fn the_epoch_and_a_few_known_dates_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2026, 1, 1), 20454);
        assert_eq!(civil_from_days(20454), (2026, 1, 1));
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(days_from_civil(1, 1, 1), -719_162);
        assert_eq!(civil_from_days(-719_162), (1, 1, 1));
        assert_eq!(days_from_civil(9999, 12, 31), 2_932_896);
    }

    #[test]
    fn leap_years_are_handled() {
        assert_eq!(
            days_from_civil(2024, 2, 29),
            days_from_civil(2024, 3, 1) - 1
        );
        // 2000 is a leap year (divisible by 400); 1900 and 2100 are not.
        assert_eq!(
            days_from_civil(2000, 2, 29),
            days_from_civil(2000, 3, 1) - 1
        );
        assert_eq!(
            days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28),
            1
        );
    }

    #[test]
    fn weekdays_match_the_known_epoch() {
        assert_eq!(weekday_from_days(0), 4, "1970-01-01 was a Thursday");
        assert_eq!(
            weekday_from_days(days_from_civil(2026, 7, 26)),
            7,
            "2026-07-26 is a Sunday"
        );
    }

    #[test]
    fn round_trips_hold_over_a_wide_range() {
        for z in (-800_000..800_000).step_by(4001) {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z, "{y}-{m}-{d}");
        }
    }
}
