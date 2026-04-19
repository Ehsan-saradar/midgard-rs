//! Query-string parsing.
//!
//! Every history endpoint takes the same `interval`/`count`/`from`/`to` parameters, so they are
//! parsed once here and handed to [`midgard_db::buckets::generate`].
//!
//! Unrecognised parameters are rejected rather than ignored. A silently-dropped `?intervall=day`
//! returns a perfectly valid response for a different question, and the caller has no way to
//! tell — upstream does the same for the same reason.

use std::collections::HashMap;

use midgard_core::{Error, Result, Second};
use midgard_db::buckets::{BucketParams, Interval};

/// A parsed query string that tracks which parameters have been consumed.
pub struct Params {
    values: HashMap<String, String>,
    consumed: Vec<String>,
}

impl Params {
    pub fn new(values: HashMap<String, String>) -> Params {
        Params {
            values,
            consumed: Vec::new(),
        }
    }

    /// Take a parameter, marking it as understood.
    pub fn take(&mut self, key: &str) -> Option<&str> {
        self.consumed.push(key.to_string());
        self.values
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    pub fn take_i64(&mut self, key: &str) -> Result<Option<i64>> {
        match self.take(key) {
            None => Ok(None),
            Some(v) => v
                .parse()
                .map(Some)
                .map_err(|_| Error::bad_request(format!("'{key}' is not an integer: {v}"))),
        }
    }

    pub fn take_second(&mut self, key: &str) -> Result<Option<Second>> {
        Ok(self.take_i64(key)?.map(Second))
    }

    pub fn take_string(&mut self, key: &str) -> Option<String> {
        self.take(key).map(str::to_string)
    }

    /// Fail if anything was passed that we did not look at.
    pub fn reject_unknown(&self) -> Result<()> {
        let unknown: Vec<&str> = self
            .values
            .keys()
            .map(String::as_str)
            .filter(|k| !self.consumed.iter().any(|c| c == k))
            .collect();

        if unknown.is_empty() {
            return Ok(());
        }
        let mut names: Vec<&str> = unknown;
        names.sort_unstable();
        Err(Error::bad_request(format!(
            "unknown query parameter(s): {}",
            names.join(", ")
        )))
    }

    /// Parse the four bucket parameters.
    pub fn buckets(&mut self) -> Result<BucketParams> {
        let from = self.take_second("from")?;
        let to = self.take_second("to")?;
        let count = self.take_i64("count")?;
        let interval = match self.take("interval") {
            None => None,
            Some(raw) => Some(Interval::parse(raw).ok_or_else(|| {
                Error::bad_request(format!(
                    "invalid interval '{raw}', accepted values: 5min, hour, day, week, month, \
                     quarter, year"
                ))
            })?),
        };
        Ok(BucketParams {
            from,
            to,
            count,
            interval,
        })
    }
}

impl From<HashMap<String, String>> for Params {
    fn from(values: HashMap<String, String>) -> Params {
        Params::new(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> Params {
        Params::new(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn parses_the_bucket_parameters() {
        let mut p = params(&[("interval", "day"), ("count", "10"), ("to", "1609459200")]);
        let b = p.buckets().unwrap();
        assert_eq!(b.interval, Some(Interval::Day));
        assert_eq!(b.count, Some(10));
        assert_eq!(b.to, Some(Second(1_609_459_200)));
        assert_eq!(b.from, None);
        p.reject_unknown().unwrap();
    }

    #[test]
    fn interval_names_are_case_insensitive() {
        let mut p = params(&[("interval", "DAY")]);
        assert_eq!(p.buckets().unwrap().interval, Some(Interval::Day));
    }

    #[test]
    fn a_bad_interval_names_the_options() {
        let mut p = params(&[("interval", "fortnight")]);
        let err = p.buckets().unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert!(err.to_string().contains("5min"), "{err}");
    }

    #[test]
    fn a_non_numeric_count_is_rejected() {
        let mut p = params(&[("count", "ten")]);
        let err = p.buckets().unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert!(err.to_string().contains("count"), "{err}");
    }

    #[test]
    fn empty_values_read_as_absent() {
        // "?from=" is what an unfilled form field sends; treating it as an error is unhelpful.
        let mut p = params(&[("from", "")]);
        assert_eq!(p.buckets().unwrap().from, None);
    }

    #[test]
    fn typos_are_rejected_rather_than_ignored() {
        // The whole point: ?intervall=day would otherwise return a valid answer to a different
        // question, with nothing to tell the caller.
        let mut p = params(&[("intervall", "day")]);
        p.buckets().unwrap();
        let err = p.reject_unknown().unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert!(err.to_string().contains("intervall"), "{err}");
    }

    #[test]
    fn several_unknown_parameters_are_listed_in_order() {
        let mut p = params(&[("zebra", "1"), ("apple", "2")]);
        p.buckets().unwrap();
        let err = p.reject_unknown().unwrap_err();
        assert!(err.to_string().contains("apple, zebra"), "{err}");
    }

    #[test]
    fn consuming_a_parameter_stops_it_being_unknown() {
        let mut p = params(&[("pool", "BTC.BTC")]);
        assert_eq!(p.take_string("pool").as_deref(), Some("BTC.BTC"));
        p.reject_unknown().unwrap();
    }

    #[test]
    fn asking_for_an_absent_parameter_still_marks_it_known() {
        let mut p = params(&[]);
        assert_eq!(p.take("limit"), None);
        p.reject_unknown().unwrap();
    }
}
