//! Block time.
//!
//! The chain timestamps blocks in nanoseconds and the API talks in seconds, and mixing the two
//! up silently produces results that are wrong by a factor of a billion. Both are newtypes so
//! the compiler catches it.
//!
//! Note the asymmetry: `Second -> Nano` is exact, `Nano -> Second` truncates. That matches the
//! database, where `block_timestamp` columns are nanosecond `BIGINT`s and every bucket boundary
//! is a whole second.

use std::fmt;
use std::ops::{Add, Sub};

pub const NANOS_PER_SECOND: i64 = 1_000_000_000;

/// Seconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Second(pub i64);

/// Nanoseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Nano(pub i64);

impl Second {
    pub const fn to_nano(self) -> Nano {
        Nano(self.0 * NANOS_PER_SECOND)
    }

    pub const fn to_i64(self) -> i64 {
        self.0
    }
}

impl Nano {
    /// Truncating, not rounding: a bucket starting mid-second belongs to that second.
    pub const fn to_second(self) -> Second {
        Second(self.0 / NANOS_PER_SECOND)
    }

    pub const fn to_i64(self) -> i64 {
        self.0
    }
}

impl Add<i64> for Second {
    type Output = Second;
    fn add(self, rhs: i64) -> Second {
        Second(self.0 + rhs)
    }
}

impl Sub<i64> for Second {
    type Output = Second;
    fn sub(self, rhs: i64) -> Second {
        Second(self.0 - rhs)
    }
}

impl Sub<Second> for Second {
    type Output = i64;
    fn sub(self, rhs: Second) -> i64 {
        self.0 - rhs.0
    }
}

impl fmt::Display for Second {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for Nano {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<i64> for Second {
    fn from(v: i64) -> Second {
        Second(v)
    }
}

impl From<i64> for Nano {
    fn from(v: i64) -> Nano {
        Nano(v)
    }
}

/// Periods per year for a window, used to annualise an interval's earnings.
///
/// Returns zero for a zero-length window so callers do not have to guard against a division that
/// only happens when a bucket is degenerate.
pub fn periods_per_year(from: Second, to: Second) -> f64 {
    let span = to.to_nano().to_i64() - from.to_nano().to_i64();
    if span <= 0 {
        return 0.0;
    }
    365.0 * 24.0 * 60.0 * 60.0 * 1e9 / span as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_to_nanos_is_exact() {
        assert_eq!(Second(1).to_nano(), Nano(1_000_000_000));
    }

    #[test]
    fn nanos_to_seconds_truncates() {
        assert_eq!(Nano(1_999_999_999).to_second(), Second(1));
        assert_eq!(Nano(999_999_999).to_second(), Second(0));
    }

    #[test]
    fn arithmetic_stays_in_seconds() {
        assert_eq!(Second(100) + 5, Second(105));
        assert_eq!(Second(100) - 5, Second(95));
        assert_eq!(Second(100) - Second(95), 5);
    }

    #[test]
    fn a_year_long_window_is_one_period() {
        let year = 365 * 24 * 60 * 60;
        assert!((periods_per_year(Second(0), Second(year)) - 1.0).abs() < 1e-9);
        assert!((periods_per_year(Second(0), Second(year / 2)) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn degenerate_window_annualises_to_zero() {
        assert_eq!(periods_per_year(Second(5), Second(5)), 0.0);
        assert_eq!(periods_per_year(Second(5), Second(1)), 0.0);
    }
}
