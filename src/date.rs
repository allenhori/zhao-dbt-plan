//! A minimal `YYYY-MM-DD` calendar date, with the day-count arithmetic
//! this addon actually needs (add/subtract N days, today/yesterday).
//!
//! Hand-rolled rather than pulling in a date crate -- same precedent
//! `zhao-cli`'s own `log.rs` already set for exactly this reason (a
//! `YYYY-MM-DD` day-count is the only thing needed here, not a general
//! date/time library). Uses Howard Hinnant's well-known
//! `days_from_civil`/`civil_from_days` algorithms -- see
//! <http://howardhinnant.github.io/date_algorithms.html>.

use std::fmt;

use serde::Deserialize;

/// The unit a `lookback`/`lookahead` amount in `config.meta.zhao` is
/// measured in -- e.g. `lookback_unit: month` for a model that needs "3
/// months back," not "3 days back." Defaults to [`TimeUnit::Day`] when
/// omitted, so the common case (`lookback: 3`, no unit at all) needs no
/// extra config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeUnit {
    /// A calendar day.
    #[default]
    Day,
    /// 7 calendar days.
    Week,
    /// A calendar month -- see [`Date::minus`]/[`Date::plus`] for how a
    /// day-of-month that doesn't exist in the target month (e.g. the
    /// 31st, one month before a 30-day month) is handled.
    Month,
    /// A calendar year (12 months).
    Year,
}

/// A calendar date, stored as a day count since the Unix epoch
/// (1970-01-01) -- the same representation
/// [`days_from_civil`]/[`civil_from_days`] convert to/from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date(i64);

/// Everything that can go wrong parsing a `--event-time-start`/
/// `--event-time-end` value.
#[derive(Debug, thiserror::Error)]
#[error("{0:?} is not a valid YYYY-MM-DD date")]
pub struct DateParseError(String);

impl Date {
    /// Parses a `YYYY-MM-DD` string. Deliberately strict: no other format
    /// is accepted, since this is the one format both this addon's own
    /// flags and dbt's `--event-time-start`/`--event-time-end` use.
    pub fn parse(s: &str) -> Result<Date, DateParseError> {
        let mut parts = s.splitn(3, '-');
        let (Some(y), Some(m), Some(d)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(DateParseError(s.to_string()));
        };
        if parts.next().is_some() {
            return Err(DateParseError(s.to_string()));
        }
        let (Ok(y), Ok(m), Ok(d)) = (y.parse::<i64>(), m.parse::<u32>(), d.parse::<u32>()) else {
            return Err(DateParseError(s.to_string()));
        };
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return Err(DateParseError(s.to_string()));
        }
        Ok(Date(days_from_civil(y, m, d)))
    }

    /// Constructs a `Date` directly from a day count since the Unix
    /// epoch -- used by callers that already have one (e.g. rendering a
    /// timestamp from `SystemTime`), rather than round-tripping through
    /// `parse`/`to_string`.
    pub fn from_days_since_epoch(days: i64) -> Date {
        Date(days)
    }

    /// Yesterday's date -- the default Anchor window (§4 of the spec) for
    /// a `zhao dbt-plan` invocation with no explicit
    /// `--event-time-start`/`--event-time-end`.
    pub fn yesterday() -> Date {
        Date(today_days() - 1)
    }

    /// This date, `days` days earlier (or later, if negative).
    pub fn minus_days(self, days: i64) -> Date {
        Date(self.0 - days)
    }

    /// This date, `days` days later (or earlier, if negative).
    pub fn plus_days(self, days: i64) -> Date {
        Date(self.0 + days)
    }

    /// This date, `amount` `unit`s earlier (or later, if negative) --
    /// the general form [`minus_days`](Self::minus_days) is a special
    /// case of. `Week` is a fixed 7 days; `Month`/`Year` are genuine
    /// calendar arithmetic (see [`add_months`](Self::add_months)), not a
    /// fixed day count.
    pub fn minus(self, amount: i64, unit: TimeUnit) -> Date {
        match unit {
            TimeUnit::Day => self.minus_days(amount),
            TimeUnit::Week => self.minus_days(amount * 7),
            TimeUnit::Month => self.add_months(-amount),
            TimeUnit::Year => self.add_months(-amount * 12),
        }
    }

    /// This date, `amount` `unit`s later (or earlier, if negative) --
    /// see [`minus`](Self::minus).
    pub fn plus(self, amount: i64, unit: TimeUnit) -> Date {
        match unit {
            TimeUnit::Day => self.plus_days(amount),
            TimeUnit::Week => self.plus_days(amount * 7),
            TimeUnit::Month => self.add_months(amount),
            TimeUnit::Year => self.add_months(amount * 12),
        }
    }

    /// This date, `months` calendar months later (or earlier, if
    /// negative). If the current day-of-month doesn't exist in the
    /// target month (e.g. adding 1 month to Jan 31, and February has no
    /// 31st), clamps to the target month's actual last day -- the same
    /// convention most calendar libraries use, rather than overflowing
    /// into the following month.
    fn add_months(self, months: i64) -> Date {
        let (y, m, d) = civil_from_days(self.0);
        let total_months = y * 12 + (m as i64 - 1) + months;
        let new_y = total_months.div_euclid(12);
        let new_m = (total_months.rem_euclid(12) + 1) as u32;
        let new_d = d.min(days_in_month(new_y, new_m));
        Date(days_from_civil(new_y, new_m, new_d))
    }

    /// The number of days from `self` to `other`, inclusive of both ends
    /// -- e.g. the same date to itself is a 1-day span, matching how a
    /// single-day Anchor window is described throughout the spec.
    pub fn span_days_to(self, other: Date) -> i64 {
        (other.0 - self.0) + 1
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (y, m, d) = civil_from_days(self.0);
        write!(f, "{y:04}-{m:02}-{d:02}")
    }
}

impl serde::Serialize for Date {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

/// Today's date (local system clock) as a day count since the Unix epoch.
fn today_days() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86_400) as i64)
        .unwrap_or(0)
}

/// Howard Hinnant's `days_from_civil`: converts a proleptic-Gregorian
/// (year, month, day) into a day count since the Unix epoch (1970-01-01).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = (m as u64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Howard Hinnant's `civil_from_days`: converts a day count since the
/// Unix epoch (1970-01-01) into a proleptic-Gregorian (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The number of days in proleptic-Gregorian month `m` of year `y`
/// (leap years handled via the standard divisible-by-4-except-100-
/// unless-400 rule).
fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
            if leap { 29 } else { 28 }
        }
        _ => unreachable!("month is always 1..=12, produced by civil_from_days"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays_round_trip() {
        let d = Date::parse("2026-07-01").expect("should parse");
        assert_eq!(d.to_string(), "2026-07-01");
    }

    #[test]
    fn rejects_an_invalid_month() {
        assert!(Date::parse("2026-13-01").is_err());
    }

    #[test]
    fn rejects_a_malformed_string() {
        assert!(Date::parse("2026/07/01").is_err());
        assert!(Date::parse("not-a-date").is_err());
    }

    #[test]
    fn minus_days_crosses_a_month_boundary() {
        let d = Date::parse("2026-07-01").expect("should parse");
        assert_eq!(d.minus_days(4).to_string(), "2026-06-27");
    }

    #[test]
    fn plus_days_crosses_a_month_boundary() {
        let d = Date::parse("2026-07-28").expect("should parse");
        assert_eq!(d.plus_days(4).to_string(), "2026-08-01");
    }

    #[test]
    fn lookback_and_lookahead_expand_in_their_own_correct_direction() {
        // The original requirement dump's own worked example for this
        // (§3.2: Model B, lookback=3/lookahead=4, claimed to expand
        // [2026-07-01, 2026-07-01] to [2026-06-27, 2026-07-05], "9 days")
        // turned out to be internally inconsistent -- it applied the
        // *lookahead* value (4) to the backward Start calculation too,
        // instead of lookback (3). Caught here via TDD, independently
        // verified against Python's `datetime`, and corrected: lookback
        // moves Start backward by its own value, lookahead moves End
        // forward by its own value, using each one exactly once.
        let start = Date::parse("2026-07-01").expect("should parse");
        let end = Date::parse("2026-07-01").expect("should parse");
        let expanded_start = start.minus_days(3);
        let expanded_end = end.plus_days(4);
        assert_eq!(expanded_start.to_string(), "2026-06-28");
        assert_eq!(expanded_end.to_string(), "2026-07-05");
        assert_eq!(expanded_start.span_days_to(expanded_end), 8);
    }

    #[test]
    fn single_day_span_is_one_not_zero() {
        let d = Date::parse("2026-07-01").expect("should parse");
        assert_eq!(d.span_days_to(d), 1);
    }

    #[test]
    fn week_unit_is_seven_days() {
        let d = Date::parse("2026-07-01").expect("should parse");
        assert_eq!(d.plus(2, TimeUnit::Week).to_string(), "2026-07-15");
        assert_eq!(d.minus(2, TimeUnit::Week).to_string(), "2026-06-17");
    }

    #[test]
    fn month_unit_crosses_a_year_boundary() {
        let d = Date::parse("2026-01-15").expect("should parse");
        assert_eq!(d.minus(2, TimeUnit::Month).to_string(), "2025-11-15");
        assert_eq!(d.plus(2, TimeUnit::Month).to_string(), "2026-03-15");
    }

    #[test]
    fn month_unit_clamps_a_day_that_does_not_exist_in_the_target_month() {
        // Jan 31 - 1 month: February has no 31st, clamp to the 28th
        // (2026 isn't a leap year).
        let d = Date::parse("2026-01-31").expect("should parse");
        assert_eq!(d.minus(1, TimeUnit::Month).to_string(), "2025-12-31");
        assert_eq!(d.plus(1, TimeUnit::Month).to_string(), "2026-02-28");
    }

    #[test]
    fn month_unit_clamps_correctly_on_a_leap_year() {
        let d = Date::parse("2024-01-31").expect("should parse");
        assert_eq!(d.plus(1, TimeUnit::Month).to_string(), "2024-02-29");
    }

    #[test]
    fn year_unit_is_twelve_months_and_respects_leap_day_clamping() {
        let d = Date::parse("2024-02-29").expect("should parse");
        assert_eq!(d.plus(1, TimeUnit::Year).to_string(), "2025-02-28");
    }

    #[test]
    fn default_unit_is_day() {
        assert_eq!(TimeUnit::default(), TimeUnit::Day);
    }
}
