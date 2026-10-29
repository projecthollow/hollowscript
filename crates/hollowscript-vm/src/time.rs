//! Calendar arithmetic on integers, as section 15.2 defines it.
//!
//! An *instant* is milliseconds since 1970-01-01T00:00:00.000Z and a *date*
//! is days since 1970-01-01, both in the proleptic Gregorian calendar, both
//! ignoring leap seconds. The library knows only UTC and fixed offsets;
//! there is no time zone database, so everything here is arithmetic with no
//! table and no ambiguity.

/// The first instant section 15.2 allows, 0001-01-01T00:00:00.000Z.
pub const MIN_INSTANT: i64 = -62_135_596_800_000;
/// The last, 9999-12-31T23:59:59.999Z.
pub const MAX_INSTANT: i64 = 253_402_300_799_999;
/// The first date it allows, 0001-01-01.
pub const MIN_DATE: i64 = -719_162;
/// The last, 9999-12-31.
pub const MAX_DATE: i64 = 2_932_896;

/// Milliseconds in a day.
const DAY: i64 = 86_400_000;

/// The UTC calendar fields of an instant (section 15.2's `TimeParts`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Parts {
    /// 1 to 9999.
    pub year: i64,
    /// 1 to 12.
    pub month: i64,
    /// 1 to the length of the month.
    pub day: i64,
    /// 0 to 23.
    pub hour: i64,
    /// 0 to 59.
    pub minute: i64,
    /// 0 to 59.
    pub second: i64,
    /// 0 to 999.
    pub millisecond: i64,
    /// 1 for Monday to 7 for Sunday.
    pub weekday: i64,
}

/// Whether `year` is a leap year in the proleptic Gregorian calendar.
#[must_use]
pub fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The number of days in `month` of `year`, or `None` for a month outside
/// 1 to 12.
#[must_use]
pub fn days_in_month(year: i64, month: i64) -> Option<i64> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        _ => return None,
    })
}

/// The date of a year, month and day, by Howard Hinnant's `days_from_civil`.
/// The fields are assumed valid; [`from_parts`] checks them first.
#[must_use]
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_shift = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_shift + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The inverse: the year, month and day of a date.
#[must_use]
pub fn civil_from_days(date: i64) -> (i64, i64, i64) {
    let shifted = date + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_shift = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_shift + 2) / 5 + 1;
    let month = if month_shift < 10 {
        month_shift + 3
    } else {
        month_shift - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// The weekday of a date, 1 for Monday to 7 for Sunday. 1970-01-01 was a
/// Thursday, which is 4.
#[must_use]
pub fn weekday_of(date: i64) -> i64 {
    (date + 3).rem_euclid(7) + 1
}

/// `time.parts` (section 15.2): the UTC calendar fields of an instant.
///
/// "It uses floor division, so `time.parts(-1)` is 1969-12-31 at
/// 23:59:59.999."
///
/// # Errors
///
/// `None` for an instant outside the valid range, which the caller reports
/// as HS1011.
#[must_use]
pub fn parts(instant: i64) -> Option<Parts> {
    if !(MIN_INSTANT..=MAX_INSTANT).contains(&instant) {
        return None;
    }
    let date = instant.div_euclid(DAY);
    let within = instant.rem_euclid(DAY);
    let (year, month, day) = civil_from_days(date);
    Some(Parts {
        year,
        month,
        day,
        hour: within / 3_600_000,
        minute: (within / 60_000) % 60,
        second: (within / 1_000) % 60,
        millisecond: within % 1_000,
        weekday: weekday_of(date),
    })
}

/// `time.fromParts` (section 15.2). `weekday` is ignored.
///
/// # Errors
///
/// `None` if any field is out of its range or names a day that does not
/// exist, or the result lies outside the valid instant range, all of which
/// the caller reports as `Err("invalid time parts")`.
#[must_use]
pub fn from_parts(parts: &Parts) -> Option<i64> {
    if !(1..=9999).contains(&parts.year)
        || !(0..24).contains(&parts.hour)
        || !(0..60).contains(&parts.minute)
        || !(0..60).contains(&parts.second)
        || !(0..1000).contains(&parts.millisecond)
    {
        return None;
    }
    let length = days_in_month(parts.year, parts.month)?;
    if !(1..=length).contains(&parts.day) {
        return None;
    }
    let date = days_from_civil(parts.year, parts.month, parts.day);
    let instant = date
        .checked_mul(DAY)?
        .checked_add(parts.hour * 3_600_000 + parts.minute * 60_000 + parts.second * 1_000)?
        .checked_add(parts.millisecond)?;
    if !(MIN_INSTANT..=MAX_INSTANT).contains(&instant) {
        return None;
    }
    Some(instant)
}

/// `time.format` (section 15.2): `YYYY-MM-DDTHH:MM:SSZ`, or with exactly
/// three fractional digits when the instant has milliseconds.
///
/// # Errors
///
/// `None` outside the valid range, which the caller reports as HS1011.
#[must_use]
pub fn format(instant: i64) -> Option<String> {
    let p = parts(instant)?;
    let head = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        p.year, p.month, p.day, p.hour, p.minute, p.second
    );
    Some(if p.millisecond == 0 {
        format!("{head}Z")
    } else {
        format!("{head}.{:03}Z", p.millisecond)
    })
}

/// `time.formatDate` (section 15.2): `YYYY-MM-DD`.
///
/// # Errors
///
/// `None` outside the valid date range, which the caller reports as HS1011.
#[must_use]
pub fn format_date(date: i64) -> Option<String> {
    if !(MIN_DATE..=MAX_DATE).contains(&date) {
        return None;
    }
    let (year, month, day) = civil_from_days(date);
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

/// Why a timestamp could not be parsed. The two messages are section 15.2's
/// own, verbatim.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TimeError {
    /// The text does not have the right form, or a field is not valid.
    Invalid,
    /// The form is right and the instant is outside the valid range.
    OutOfRange,
}

/// `time.parse` (section 15.2): an RFC 3339 date-time to a UTC instant.
///
/// # Errors
///
/// [`TimeError::Invalid`] for text that does not have the stated form or a
/// field that is not valid, and [`TimeError::OutOfRange`] when the instant
/// that results lies outside the valid range.
pub fn parse(text: &str) -> Result<i64, TimeError> {
    let bytes = text.as_bytes();
    // YYYY-MM-DDTHH:MM:SS is 19 bytes, then the fraction, then the offset.
    if bytes.len() < 20 {
        return Err(TimeError::Invalid);
    }
    let year = digits(bytes, 0, 4)?;
    expect(bytes, 4, b'-')?;
    let month = digits(bytes, 5, 2)?;
    expect(bytes, 7, b'-')?;
    let day = digits(bytes, 8, 2)?;
    expect(bytes, 10, b'T')?;
    let hour = digits(bytes, 11, 2)?;
    expect(bytes, 13, b':')?;
    let minute = digits(bytes, 14, 2)?;
    expect(bytes, 16, b':')?;
    let second = digits(bytes, 17, 2)?;

    let mut at = 19;
    let mut millisecond = 0;
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while matches!(bytes.get(at), Some(b'0'..=b'9')) {
            at += 1;
        }
        let count = at - start;
        // "optionally `.` and 1 to 9 fractional digits"
        if !(1..=9).contains(&count) {
            return Err(TimeError::Invalid);
        }
        // "Fractional digits beyond the third are truncated."
        for offset in 0..3 {
            let digit = bytes
                .get(start + offset)
                .filter(|b| b.is_ascii_digit())
                .map_or(0, |b| i64::from(b - b'0'));
            millisecond = millisecond * 10 + digit;
        }
    }

    let offset_minutes = match bytes.get(at) {
        Some(b'Z') => {
            at += 1;
            0
        }
        Some(sign @ (b'+' | b'-')) => {
            let sign = if *sign == b'-' { -1 } else { 1 };
            let offset_hour = digits(bytes, at + 1, 2)?;
            expect(bytes, at + 3, b':')?;
            let offset_minute = digits(bytes, at + 4, 2)?;
            at += 6;
            if offset_hour >= 24 || offset_minute >= 60 {
                return Err(TimeError::Invalid);
            }
            sign * (offset_hour * 60 + offset_minute)
        }
        _ => return Err(TimeError::Invalid),
    };
    if at != bytes.len() {
        return Err(TimeError::Invalid);
    }

    // "a leap second `:60` is not valid"
    if hour >= 24 || minute >= 60 || second >= 60 {
        return Err(TimeError::Invalid);
    }
    let Some(length) = days_in_month(year, month) else {
        return Err(TimeError::Invalid);
    };
    if !(1..=length).contains(&day) {
        return Err(TimeError::Invalid);
    }
    // "which includes a year 0000 in the text, although the fields are
    // otherwise valid": year 0 is out of range rather than invalid.
    if year == 0 {
        return Err(TimeError::OutOfRange);
    }
    let local = days_from_civil(year, month, day) * DAY
        + hour * 3_600_000
        + minute * 60_000
        + second * 1_000
        + millisecond;
    // "The result is the UTC instant, that is the local time minus the
    // offset."
    let instant = local - offset_minutes * 60_000;
    if !(MIN_INSTANT..=MAX_INSTANT).contains(&instant) {
        return Err(TimeError::OutOfRange);
    }
    Ok(instant)
}

/// `time.parseDate` (section 15.2): exactly `YYYY-MM-DD`.
///
/// # Errors
///
/// [`TimeError::Invalid`] for anything else, including `0000-01-01`.
pub fn parse_date(text: &str) -> Result<i64, TimeError> {
    let bytes = text.as_bytes();
    if bytes.len() != 10 {
        return Err(TimeError::Invalid);
    }
    let year = digits(bytes, 0, 4)?;
    expect(bytes, 4, b'-')?;
    let month = digits(bytes, 5, 2)?;
    expect(bytes, 7, b'-')?;
    let day = digits(bytes, 8, 2)?;
    // "a year from 0001 to 9999 ... (so `0000-01-01` is invalid)"
    if year == 0 {
        return Err(TimeError::Invalid);
    }
    let Some(length) = days_in_month(year, month) else {
        return Err(TimeError::Invalid);
    };
    if !(1..=length).contains(&day) {
        return Err(TimeError::Invalid);
    }
    Ok(days_from_civil(year, month, day))
}

/// `count` ASCII digits at `at`, as a number.
fn digits(bytes: &[u8], at: usize, count: usize) -> Result<i64, TimeError> {
    let slice = bytes.get(at..at + count).ok_or(TimeError::Invalid)?;
    let mut value = 0i64;
    for byte in slice {
        if !byte.is_ascii_digit() {
            return Err(TimeError::Invalid);
        }
        value = value * 10 + i64::from(byte - b'0');
    }
    Ok(value)
}

fn expect(bytes: &[u8], at: usize, wanted: u8) -> Result<(), TimeError> {
    if bytes.get(at) == Some(&wanted) {
        Ok(())
    } else {
        Err(TimeError::Invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_DATE, MAX_INSTANT, MIN_DATE, MIN_INSTANT, Parts, TimeError, civil_from_days,
        days_from_civil, format, format_date, from_parts, parse, parse_date, parts, weekday_of,
    };

    /// The range endpoints section 15.2 names, to the millisecond.
    #[test]
    fn the_range_endpoints_are_the_ones_section_15_2_names() {
        assert_eq!(format(MIN_INSTANT).as_deref(), Some("0001-01-01T00:00:00Z"));
        assert_eq!(
            format(MAX_INSTANT).as_deref(),
            Some("9999-12-31T23:59:59.999Z")
        );
        assert_eq!(format(MIN_INSTANT - 1), None);
        assert_eq!(format(MAX_INSTANT + 1), None);
        assert_eq!(format_date(MIN_DATE).as_deref(), Some("0001-01-01"));
        assert_eq!(format_date(MAX_DATE).as_deref(), Some("9999-12-31"));
        assert_eq!(format_date(MIN_DATE - 1), None);
        assert_eq!(format_date(MAX_DATE + 1), None);
    }

    #[test]
    fn the_epoch_and_its_neighbours() {
        assert_eq!(format(0).as_deref(), Some("1970-01-01T00:00:00Z"));
        // "It uses floor division, so `time.parts(-1)` is 1969-12-31 at
        // 23:59:59.999."
        let p = parts(-1).expect("in range");
        assert_eq!((p.year, p.month, p.day), (1969, 12, 31));
        assert_eq!(
            (p.hour, p.minute, p.second, p.millisecond),
            (23, 59, 59, 999)
        );
        // 1970-01-01 was a Thursday.
        assert_eq!(weekday_of(0), 4);
        assert_eq!(weekday_of(3), 7, "Sunday");
        assert_eq!(weekday_of(4), 1, "Monday");
    }

    /// The worked example of section 15.2.
    #[test]
    fn the_worked_example_of_section_15_2() {
        let instant = parse("2026-07-26T09:15:00+01:00").expect("valid");
        // "The result is the UTC instant, that is the local time minus the
        // offset."
        assert_eq!(format(instant).as_deref(), Some("2026-07-26T08:15:00Z"));
        let p = parts(instant).expect("in range");
        assert_eq!((p.year, p.month, p.weekday), (2026, 7, 7), "a Sunday");
        assert_eq!(parse_date("2026-13-45"), Err(TimeError::Invalid));
    }

    #[test]
    fn parse_accepts_only_the_form_section_15_2_states() {
        assert!(parse("2026-01-01T00:00:00Z").is_ok());
        assert!(parse("2026-01-01T00:00:00.5Z").is_ok());
        assert!(parse("2026-01-01T00:00:00.123456789Z").is_ok());
        assert!(
            parse("2026-01-01T00:00:00-00:00").is_ok(),
            "-00:00 is +00:00"
        );
        assert_eq!(
            parse("2026-01-01T00:00:00.5Z").ok(),
            parse("2026-01-01T00:00:00.500Z").ok()
        );
        assert_eq!(
            parse("2026-01-01T00:00:00.123999Z").ok(),
            parse("2026-01-01T00:00:00.123Z").ok(),
            "digits beyond the third are truncated"
        );
        for bad in [
            "2026-01-01 00:00:00Z",
            "2026-01-01t00:00:00Z",
            "2026-01-01T00:00:00",
            "2026-01-01T00:00:00.Z",
            "2026-01-01T00:00:00.1234567890Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:60:00Z",
            "2026-01-01T00:00:60Z",
            "2026-02-30T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-01-01T00:00:00+24:00",
            "2026-01-01T00:00:00+00:60",
            " 2026-01-01T00:00:00Z",
            "2026-01-01T00:00:00Z ",
        ] {
            assert_eq!(parse(bad), Err(TimeError::Invalid), "{bad}");
        }
        // "a year 0000 in the text, although the fields are otherwise valid"
        assert_eq!(parse("0000-01-01T00:00:00Z"), Err(TimeError::OutOfRange));
    }

    #[test]
    fn leap_years_are_proleptic_gregorian() {
        assert!(parse_date("2024-02-29").is_ok());
        assert_eq!(parse_date("2023-02-29"), Err(TimeError::Invalid));
        assert!(parse_date("2000-02-29").is_ok(), "divisible by 400");
        assert_eq!(parse_date("1900-02-29"), Err(TimeError::Invalid), "by 100");
        assert_eq!(parse_date("0000-01-01"), Err(TimeError::Invalid));
    }

    #[test]
    fn from_parts_checks_every_field_and_ignores_the_weekday() {
        let day = Parts {
            year: 2026,
            month: 2,
            day: 29,
            hour: 0,
            minute: 0,
            second: 0,
            millisecond: 0,
            // Deliberately wrong: "weekday is ignored".
            weekday: 3,
        };
        assert_eq!(from_parts(&day), None, "February 30 does not exist");
        let good = Parts { day: 28, ..day };
        let instant = from_parts(&good).expect("valid");
        assert_eq!(format(instant).as_deref(), Some("2026-02-28T00:00:00Z"));
        for bad in [
            Parts { year: 0, ..good },
            Parts {
                year: 10000,
                ..good
            },
            Parts { month: 0, ..good },
            Parts { month: 13, ..good },
            Parts { day: 0, ..good },
            Parts { hour: 24, ..good },
            Parts { minute: 60, ..good },
            Parts { second: 60, ..good },
            Parts {
                millisecond: 1000,
                ..good
            },
        ] {
            assert_eq!(from_parts(&bad), None, "{bad:?}");
        }
    }

    /// Every date of the valid range round-trips through the civil
    /// conversion, which is the property the whole section rests on.
    #[test]
    fn civil_conversion_round_trips_across_the_whole_range() {
        let mut date = MIN_DATE;
        while date <= MAX_DATE {
            let (year, month, day) = civil_from_days(date);
            assert_eq!(days_from_civil(year, month, day), date, "{date}");
            // A stride that is not a divisor of any cycle, so the walk hits
            // every weekday, month length and leap rule.
            date += 997;
        }
        for date in [MIN_DATE, -1, 0, 1, MAX_DATE] {
            let (y, m, d) = civil_from_days(date);
            assert_eq!(days_from_civil(y, m, d), date);
        }
    }

    #[test]
    fn parts_and_from_parts_are_inverses() {
        let mut instant = MIN_INSTANT;
        while instant < MAX_INSTANT - 1_000_000_000 {
            let p = parts(instant).expect("in range");
            assert_eq!(from_parts(&p), Some(instant), "{instant}");
            instant += 86_400_000 * 997 + 3_661_001;
        }
    }
}
