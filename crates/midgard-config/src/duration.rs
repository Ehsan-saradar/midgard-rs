//! Go-style duration strings.
//!
//! The upstream config files are full of values like `"8s"`, `"7000ms"` and `"1h30m"`, and the
//! SREs running mainnet Midgard have those files memorised. Accepting a different spelling would
//! be a gratuitous migration, so this parses Go's `time.ParseDuration` grammar:
//!
//! ```text
//! duration :≡ [sign] ( number unit )+
//! unit     :≡ ns | us | µs | ms | s | m | h
//! ```

use std::fmt;
use std::time::Duration as StdDuration;

use serde::de::{self, Deserialize, Deserializer};
use serde::{Serialize, Serializer};

/// A duration that deserialises from a Go duration string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Duration(pub StdDuration);

impl Duration {
    pub const fn from_secs(s: u64) -> Duration {
        Duration(StdDuration::from_secs(s))
    }

    pub const fn from_millis(ms: u64) -> Duration {
        Duration(StdDuration::from_millis(ms))
    }

    pub const fn get(self) -> StdDuration {
        self.0
    }
}

impl From<Duration> for StdDuration {
    fn from(d: Duration) -> StdDuration {
        d.0
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid duration {input:?}: {reason}")]
pub struct ParseError {
    input: String,
    reason: &'static str,
}

fn unit_nanos(unit: &str) -> Option<u128> {
    Some(match unit {
        "ns" => 1,
        "us" | "µs" | "μs" => 1_000,
        "ms" => 1_000_000,
        "s" => 1_000_000_000,
        "m" => 60 * 1_000_000_000,
        "h" => 3_600 * 1_000_000_000,
        _ => return None,
    })
}

pub fn parse(input: &str) -> Result<Duration, ParseError> {
    let err = |reason| ParseError {
        input: input.to_string(),
        reason,
    };

    let s = input.trim();
    if s.is_empty() {
        return Err(err("empty"));
    }
    // Go accepts "0" with no unit as a special case, and config files use it.
    if s == "0" {
        return Ok(Duration(StdDuration::ZERO));
    }

    let (negative, mut rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if negative {
        // A negative timeout is always a mistake rather than something to honour.
        return Err(err("negative"));
    }

    let mut total_nanos: u128 = 0;
    let mut saw_component = false;

    while !rest.is_empty() {
        let num_end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .ok_or_else(|| err("missing unit"))?;
        if num_end == 0 {
            return Err(err("missing number"));
        }
        let (num, tail) = rest.split_at(num_end);
        let value: f64 = num.parse().map_err(|_| err("bad number"))?;

        let unit_end = tail
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(tail.len());
        let (unit, tail) = tail.split_at(unit_end);
        let scale = unit_nanos(unit).ok_or_else(|| err("unknown unit"))?;

        total_nanos = total_nanos
            .checked_add((value * scale as f64) as u128)
            .ok_or_else(|| err("overflow"))?;
        saw_component = true;
        rest = tail;
    }

    if !saw_component {
        return Err(err("no components"));
    }
    let secs = (total_nanos / 1_000_000_000) as u64;
    let nanos = (total_nanos % 1_000_000_000) as u32;
    Ok(Duration(StdDuration::new(secs, nanos)))
}

impl fmt::Display for Duration {
    /// Renders back into the same grammar, so a round trip through the config is lossless.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total = self.0.as_nanos();
        if total == 0 {
            return write!(f, "0s");
        }
        if total % 1_000_000_000 != 0 {
            if total % 1_000_000 == 0 {
                return write!(f, "{}ms", total / 1_000_000);
            }
            return write!(f, "{total}ns");
        }
        let secs = total / 1_000_000_000;
        if secs % 3600 == 0 {
            write!(f, "{}h", secs / 3600)
        } else if secs % 60 == 0 {
            write!(f, "{}m", secs / 60)
        } else {
            write!(f, "{secs}s")
        }
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let s = String::deserialize(d)?;
        parse(&s).map_err(de::Error::custom)
    }
}

impl Serialize for Duration {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str) -> u128 {
        parse(s).unwrap().0.as_millis()
    }

    #[test]
    fn single_unit() {
        assert_eq!(ms("8s"), 8_000);
        assert_eq!(ms("7000ms"), 7_000);
        assert_eq!(ms("2m"), 120_000);
        assert_eq!(ms("1h"), 3_600_000);
        assert_eq!(parse("500us").unwrap().0.as_micros(), 500);
        assert_eq!(parse("10ns").unwrap().0.as_nanos(), 10);
    }

    #[test]
    fn compound_units_add_up() {
        assert_eq!(ms("1h30m"), 5_400_000);
        assert_eq!(ms("1m30s500ms"), 90_500);
    }

    #[test]
    fn fractional_values() {
        assert_eq!(ms("1.5s"), 1_500);
        assert_eq!(ms("0.5m"), 30_000);
    }

    #[test]
    fn bare_zero_is_accepted() {
        assert_eq!(ms("0"), 0);
    }

    #[test]
    fn rejects_nonsense() {
        for bad in ["", "s", "10", "10x", "-5s", "abc"] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn display_round_trips() {
        for s in ["8s", "2m", "1h", "7000ms"] {
            let d = parse(s).unwrap();
            assert_eq!(parse(&d.to_string()).unwrap(), d, "{s}");
        }
    }

    #[test]
    fn deserializes_from_json() {
        let d: Duration = serde_json::from_str("\"20s\"").unwrap();
        assert_eq!(d, Duration::from_secs(20));
        assert!(serde_json::from_str::<Duration>("\"20 seconds\"").is_err());
    }
}
