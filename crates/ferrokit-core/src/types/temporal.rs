// SPDX-License-Identifier: LGPL-3.0-only
//! Date, Time, and DateTime types (`QDate`, `QTime`, `QDateTime` equivalents).
//!
//! Provides lightweight, standard calendar date and clock time representations
//! matching Qt's temporal primitives. Dates use the proleptic Gregorian
//! calendar with astronomical year numbering (year 0 exists and is a leap
//! year), as in ISO 8601.

use std::fmt;

/// Calendar date (`QDate` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl Date {
    /// Creates a new date.
    pub const fn new(year: i32, month: u32, day: u32) -> Self {
        Self { year, month, day }
    }

    /// Returns `true` if the date is a valid Gregorian calendar date.
    pub fn is_valid(&self) -> bool {
        self.month >= 1
            && self.month <= 12
            && self.day >= 1
            && self.day <= Self::days_in_month_of(self.year, self.month)
    }

    /// Returns `true` if the year is a leap year.
    pub const fn is_leap_year(&self) -> bool {
        Self::is_leap(self.year)
    }

    /// Returns `true` if `year` is a proleptic Gregorian leap year (`QDate::isLeapYear`).
    pub const fn is_leap(year: i32) -> bool {
        (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
    }

    /// Number of days in `month` (1..=12) of `year`; 0 for an invalid month.
    pub const fn days_in_month_of(year: i32, month: u32) -> u32 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if Self::is_leap(year) {
                    29
                } else {
                    28
                }
            }
            _ => 0,
        }
    }

    /// Number of days in this date's month (`QDate::daysInMonth`).
    pub const fn days_in_month(&self) -> u32 {
        Self::days_in_month_of(self.year, self.month)
    }

    /// Number of days in this date's year (`QDate::daysInYear`).
    pub const fn days_in_year(&self) -> u32 {
        if self.is_leap_year() {
            366
        } else {
            365
        }
    }

    /// Days since the Unix epoch (1970-01-01 is day 0).
    pub fn to_epoch_days(&self) -> i64 {
        days_from_civil(self.year as i64, self.month as i64, self.day as i64)
    }

    /// Builds a date from days since the Unix epoch.
    pub fn from_epoch_days(days: i64) -> Self {
        let (year, month, day) = civil_from_days(days);
        Self::new(year, month, day)
    }

    /// Julian Day Number (`QDate::toJulianDay`); 1970-01-01 is JDN 2440588.
    pub fn to_julian_day(&self) -> i64 {
        self.to_epoch_days() + JULIAN_DAY_OF_EPOCH
    }

    /// Builds a date from a Julian Day Number (`QDate::fromJulianDay`).
    pub fn from_julian_day(jd: i64) -> Self {
        Self::from_epoch_days(jd - JULIAN_DAY_OF_EPOCH)
    }

    /// Returns the date `days` days later (earlier if negative) (`QDate::addDays`).
    pub fn add_days(&self, days: i64) -> Self {
        Self::from_epoch_days(self.to_epoch_days().saturating_add(days))
    }

    /// Adds calendar months, clamping the day to the target month's length (`QDate::addMonths`).
    pub fn add_months(&self, months: i32) -> Self {
        let total = self.year as i64 * 12 + (self.month as i64 - 1) + months as i64;
        let year = total.div_euclid(12) as i32;
        let month = total.rem_euclid(12) as u32 + 1;
        let day = self.day.min(Self::days_in_month_of(year, month));
        Self::new(year, month, day)
    }

    /// Adds calendar years; Feb 29 maps to Feb 28 in non-leap years (`QDate::addYears`).
    pub fn add_years(&self, years: i32) -> Self {
        let year = self.year.saturating_add(years);
        let day = self.day.min(Self::days_in_month_of(year, self.month));
        Self::new(year, self.month, day)
    }

    /// Number of days from this date to `other` (negative if `other` is earlier) (`QDate::daysTo`).
    pub fn days_to(&self, other: &Date) -> i64 {
        other.to_epoch_days() - self.to_epoch_days()
    }

    /// ISO weekday: 1 = Monday ... 7 = Sunday (`QDate::dayOfWeek`).
    pub fn day_of_week(&self) -> u32 {
        // 1970-01-01 was a Thursday (4).
        ((self.to_epoch_days() + 3).rem_euclid(7) + 1) as u32
    }

    /// Ordinal day within the year, 1-based (`QDate::dayOfYear`).
    pub fn day_of_year(&self) -> u32 {
        (Date::new(self.year, 1, 1).days_to(self) + 1) as u32
    }

    /// ISO 8601 week number and the ISO week-numbering year it belongs to (`QDate::weekNumber`).
    pub fn week_number(&self) -> (u32, i32) {
        // The ISO week containing this date's Thursday determines the week-year.
        let thursday = self.add_days(4 - self.day_of_week() as i64);
        let week = (thursday.day_of_year() - 1) / 7 + 1;
        (week, thursday.year)
    }

    /// Today's date in UTC (`QDate::currentDate`).
    pub fn current_date() -> Self {
        DateTime::current_date_time().date
    }

    /// Formats as standard ISO-8601 date string `YYYY-MM-DD`.
    pub fn to_iso_string(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// Clock time (`QTime` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Time {
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millisecond: u32,
}

impl Time {
    /// Creates a new clock time.
    pub const fn new(hour: u32, minute: u32, second: u32, millisecond: u32) -> Self {
        Self {
            hour,
            minute,
            second,
            millisecond,
        }
    }

    /// Returns `true` if the time components are within valid ranges.
    pub fn is_valid(&self) -> bool {
        self.hour < 24 && self.minute < 60 && self.second < 60 && self.millisecond < 1000
    }

    /// Returns the number of milliseconds since the start of the day (00:00:00.000).
    pub fn msecs_since_start_of_day(&self) -> u32 {
        // Fields are public, so out-of-range values must saturate instead of overflowing.
        let total = ((self.hour as u64 * 60 + self.minute as u64) * 60 + self.second as u64) * 1000
            + self.millisecond as u64;
        total.min(u32::MAX as u64) as u32
    }

    /// Creates a `Time` from milliseconds since the start of the day.
    pub fn from_msecs_since_start_of_day(msecs: u32) -> Self {
        let ms = msecs % 86_400_000;
        let millisecond = ms % 1000;
        let total_seconds = ms / 1000;
        let second = total_seconds % 60;
        let total_minutes = total_seconds / 60;
        let minute = total_minutes % 60;
        let hour = total_minutes / 60;
        Self {
            hour,
            minute,
            second,
            millisecond,
        }
    }

    /// Returns the time `ms` milliseconds later, wrapping around midnight (`QTime::addMSecs`).
    pub fn add_msecs(&self, ms: i64) -> Self {
        let total = (self.msecs_since_start_of_day() as i64).saturating_add(ms).rem_euclid(86_400_000);
        Self::from_msecs_since_start_of_day(total as u32)
    }

    /// Returns the time `secs` seconds later, wrapping around midnight (`QTime::addSecs`).
    pub fn add_secs(&self, secs: i64) -> Self {
        self.add_msecs(secs.saturating_mul(1000))
    }

    /// Milliseconds from this time to `other` within the same day (`QTime::msecsTo`).
    pub fn msecs_to(&self, other: &Time) -> i64 {
        other.msecs_since_start_of_day() as i64 - self.msecs_since_start_of_day() as i64
    }

    /// Whole seconds from this time to `other` within the same day (`QTime::secsTo`).
    pub fn secs_to(&self, other: &Time) -> i64 {
        (other.msecs_since_start_of_day() / 1000) as i64
            - (self.msecs_since_start_of_day() / 1000) as i64
    }

    /// Formats as standard ISO-8601 time string `HH:MM:SS.zzz`.
    pub fn to_iso_string(&self) -> String {
        format!(
            "{:02}:{:02}:{:02}.{:03}",
            self.hour, self.minute, self.second, self.millisecond
        )
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02}:{:02}:{:02}.{:03}",
            self.hour, self.minute, self.second, self.millisecond
        )
    }
}

/// Calendar date and clock time (`QDateTime` equivalent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct DateTime {
    pub date: Date,
    pub time: Time,
    pub timestamp_ms: i64,
}

impl DateTime {
    /// Creates a DateTime from date and time components.
    pub fn new(date: Date, time: Time) -> Self {
        let ts = calculate_timestamp_ms(&date, &time);
        Self {
            date,
            time,
            timestamp_ms: ts,
        }
    }

    /// Creates a DateTime directly from Unix epoch milliseconds.
    pub fn from_timestamp_ms(timestamp_ms: i64) -> Self {
        let (date, time) = timestamp_ms_to_date_time(timestamp_ms);
        Self {
            date,
            time,
            timestamp_ms,
        }
    }

    /// Returns the current DateTime from system time.
    pub fn current_date_time() -> Self {
        let now = std::time::SystemTime::now();
        let ms = match now.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_millis() as i64,
            Err(e) => -(e.duration().as_millis() as i64),
        };
        Self::from_timestamp_ms(ms)
    }

    /// Returns `true` if both date and time components are valid.
    pub fn is_valid(&self) -> bool {
        self.date.is_valid() && self.time.is_valid()
    }

    /// Returns this date-time shifted by `ms` milliseconds (`QDateTime::addMSecs`).
    pub fn add_msecs(&self, ms: i64) -> Self {
        Self::from_timestamp_ms(self.timestamp_ms.saturating_add(ms))
    }

    /// Returns this date-time shifted by `secs` seconds (`QDateTime::addSecs`).
    pub fn add_secs(&self, secs: i64) -> Self {
        self.add_msecs(secs.saturating_mul(1000))
    }

    /// Returns this date-time shifted by whole days, keeping the clock time (`QDateTime::addDays`).
    pub fn add_days(&self, days: i64) -> Self {
        Self::new(self.date.add_days(days), self.time)
    }

    /// Returns this date-time shifted by calendar months (`QDateTime::addMonths`).
    pub fn add_months(&self, months: i32) -> Self {
        Self::new(self.date.add_months(months), self.time)
    }

    /// Returns this date-time shifted by calendar years (`QDateTime::addYears`).
    pub fn add_years(&self, years: i32) -> Self {
        Self::new(self.date.add_years(years), self.time)
    }

    /// Milliseconds from this date-time to `other` (`QDateTime::msecsTo`).
    pub fn msecs_to(&self, other: &DateTime) -> i64 {
        other.timestamp_ms.saturating_sub(self.timestamp_ms)
    }

    /// Whole seconds from this date-time to `other` (`QDateTime::secsTo`).
    pub fn secs_to(&self, other: &DateTime) -> i64 {
        self.msecs_to(other) / 1000
    }

    /// Returns Unix epoch milliseconds.
    pub fn to_timestamp_ms(&self) -> i64 {
        self.timestamp_ms
    }

    /// Formats as standard ISO-8601 combined string `YYYY-MM-DDTHH:MM:SS.zzzZ`.
    pub fn to_iso_string(&self) -> String {
        format!(
            "{}T{}Z",
            self.date.to_iso_string(),
            self.time.to_iso_string()
        )
    }
}

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_iso_string())
    }
}

/// Julian Day Number of 1970-01-01.
const JULIAN_DAY_OF_EPOCH: i64 = 2_440_588;

/// Days since 1970-01-01 for a proleptic Gregorian civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Proleptic Gregorian civil date for days since 1970-01-01 (inverse of [`days_from_civil`]).
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    // Clamp so the arithmetic below cannot overflow and the year fits in i32.
    const MAX_DAYS: i64 = (i32::MAX as i64) * 366;
    let z = days.clamp(-MAX_DAYS, MAX_DAYS) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (if m <= 2 { y + 1 } else { y }).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    (year, m, d)
}

fn calculate_timestamp_ms(date: &Date, time: &Time) -> i64 {
    date.to_epoch_days()
        .saturating_mul(86_400_000)
        .saturating_add(time.msecs_since_start_of_day() as i64)
}

fn timestamp_ms_to_date_time(timestamp_ms: i64) -> (Date, Time) {
    let total_days = timestamp_ms.div_euclid(86_400_000);
    let rem_ms = timestamp_ms.rem_euclid(86_400_000) as u32;
    (
        Date::from_epoch_days(total_days),
        Time::from_msecs_since_start_of_day(rem_ms),
    )
}
