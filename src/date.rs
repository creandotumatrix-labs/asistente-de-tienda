//! Minimal std-only calendar math (no `chrono` dependency).
//! Based on Howard Hinnant's `days_from_civil` / `civil_from_days`.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub y: i32,
    pub m: u32,
    pub d: u32,
}

impl Date {
    pub fn new(y: i32, m: u32, d: u32) -> Self {
        Date { y, m, d }
    }

    /// Parse `YYYY-MM-DD`; `None` on malformed input.
    pub fn parse(s: &str) -> Option<Date> {
        let mut it = s.trim().split('-');
        let y = it.next()?.parse::<i32>().ok()?;
        let m = it.next()?.parse::<u32>().ok()?;
        let d = it.next()?.parse::<u32>().ok()?;
        if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        Some(Date { y, m, d })
    }

    /// Days since the Unix epoch (1970-01-01); may be negative.
    pub fn to_days(self) -> i64 {
        let y = self.y as i64;
        let m = self.m as i64;
        let d = self.d as i64;
        let y = if m <= 2 { y - 1 } else { y };
        let era = (if y >= 0 { y } else { y - 399 }) / 400;
        let yoe = y - era * 400; // [0, 399]
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
        era * 146097 + doe - 719468
    }

    pub fn from_days(z: i64) -> Date {
        let z = z + 719468;
        let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
        let doe = z - era * 146097; // [0, 146096]
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
        let mp = (5 * doy + 2) / 153; // [0, 11]
        let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
        let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
        Date {
            y: (if m <= 2 { y + 1 } else { y }) as i32,
            m: m as u32,
            d: d as u32,
        }
    }

    /// Today's date in UTC, read from the system clock.
    pub fn today_utc() -> Date {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Date::from_days(secs.div_euclid(86_400))
    }
}

/// Whole days from `a` to `b` (i.e. `b - a`). Positive when `b` is later.
pub fn days_between(a: Date, b: Date) -> i64 {
    b.to_days() - a.to_days()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_zero() {
        assert_eq!(Date::new(1970, 1, 1).to_days(), 0);
    }

    #[test]
    fn roundtrip() {
        for &(y, m, d) in &[(2026, 6, 29), (2000, 2, 29), (1999, 12, 31), (2026, 1, 1)] {
            let dt = Date::new(y, m, d);
            assert_eq!(Date::from_days(dt.to_days()), dt);
        }
    }

    #[test]
    fn diffs() {
        assert_eq!(days_between(Date::new(2026, 6, 20), Date::new(2026, 6, 29)), 9);
        assert_eq!(days_between(Date::new(2026, 5, 10), Date::new(2026, 6, 29)), 50);
    }
}
