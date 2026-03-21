//! Time bucketing for the history endpoints.
//!
//! Every `/v2/history/*` endpoint takes the same `interval`/`count`/`from`/`to` parameters and
//! answers with the same `meta` + `intervals` shape, so the parameter handling lives here once.
//!
//! There are two modes:
//!
//! * **with `interval`** — boundaries are snapped to calendar edges (start of hour, start of
//!   month, ...) and the result is `count + 1` timestamps, so that interval `i` spans
//!   `[timestamps[i], timestamps[i+1])` and the last timestamp closes the final bucket.
//! * **without `interval`** — exactly two timestamps, i.e. one `from..to` span, `meta` only.
//!
//! Boundaries are computed in Rust rather than by asking the database to generate a gapfilled
//! series. The Go implementation issues a `time_bucket_gapfill` query with a `WHERE 1=0` on
//! `block_pool_depths` purely to borrow postgres' `date_trunc`, which costs a round trip before
//! the real query runs; `date_trunc` semantics are reproducible here exactly, including the
//! detail that months and years are not fixed-length.

use midgard_core::{Error, Second};

use crate::block_log::BlockCursor;

/// Buckets wider than this are refused: the response would be enormous and the query behind it
/// scans proportionally.
pub const MAX_INTERVAL_COUNT: usize = 400;

/// How far outside the requested window we are willing to look for boundaries before giving up.
const CUTOFF_WINDOW_MULTIPLE: i64 = 2 * MAX_INTERVAL_COUNT as i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interval {
    Min5,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl Interval {
    pub fn parse(s: &str) -> Option<Interval> {
        Some(match s.to_ascii_lowercase().as_str() {
            "5min" => Interval::Min5,
            "hour" => Interval::Hour,
            "day" => Interval::Day,
            "week" => Interval::Week,
            "month" => Interval::Month,
            "quarter" => Interval::Quarter,
            "year" => Interval::Year,
            _ => return None,
        })
    }

    /// The name used in JSON and in aggregate view names.
    pub fn name(self) -> &'static str {
        match self {
            Interval::Min5 => "5min",
            Interval::Hour => "hour",
            Interval::Day => "day",
            Interval::Week => "week",
            Interval::Month => "month",
            Interval::Quarter => "quarter",
            Interval::Year => "year",
        }
    }

    /// The field name for postgres' `date_trunc`.
    ///
    /// `date_trunc` has no '5 minutes', so 5min buckets are produced by truncating to the minute
    /// and then rounding the epoch second down to a multiple of 300.
    pub fn date_trunc_field(self) -> &'static str {
        match self {
            Interval::Min5 => "minute",
            Interval::Hour => "hour",
            Interval::Day => "day",
            Interval::Week => "week",
            Interval::Month => "month",
            Interval::Quarter => "quarter",
            Interval::Year => "year",
        }
    }

    /// Shortest this interval can be, in seconds. Equal to [`Self::max_duration`] for the
    /// fixed-length intervals; smaller for months, quarters and years.
    pub fn min_duration(self) -> i64 {
        match self {
            Interval::Min5 => 300,
            Interval::Hour => 3_600,
            Interval::Day => 86_400,
            Interval::Week => 7 * 86_400,
            Interval::Month => 28 * 86_400,
            Interval::Quarter => 3 * 28 * 86_400,
            Interval::Year => 365 * 86_400,
        }
    }

    /// Longest this interval can be, in seconds. Used to widen a search window so that the
    /// boundary just outside it is still found.
    pub fn max_duration(self) -> i64 {
        match self {
            Interval::Min5 => 300,
            Interval::Hour => 3_600,
            Interval::Day => 86_400,
            Interval::Week => 7 * 86_400,
            Interval::Month => 31 * 86_400,
            Interval::Quarter => 3 * 31 * 86_400,
            Interval::Year => 366 * 86_400,
        }
    }
}

/// A half-open span, `[from, until)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub from: Second,
    pub until: Second,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buckets {
    /// `count() + 1` boundaries. Never fewer than two.
    timestamps: Vec<Second>,
    interval: Option<Interval>,
}

impl Buckets {
    /// A single span with no interval — the `meta`-only mode.
    pub fn one(from: Second, until: Second) -> Buckets {
        Buckets {
            timestamps: vec![from, until],
            interval: None,
        }
    }

    pub fn start(&self) -> Second {
        self.timestamps[0]
    }

    pub fn end(&self) -> Second {
        *self
            .timestamps
            .last()
            .expect("always at least two boundaries")
    }

    pub fn count(&self) -> usize {
        self.timestamps.len() - 1
    }

    pub fn timestamps(&self) -> &[Second] {
        &self.timestamps
    }

    pub fn bucket(&self, i: usize) -> Window {
        Window {
            from: self.timestamps[i],
            until: self.timestamps[i + 1],
        }
    }

    pub fn window(&self) -> Window {
        Window {
            from: self.start(),
            until: self.end(),
        }
    }

    pub fn interval(&self) -> Option<Interval> {
        self.interval
    }

    pub fn is_one_interval(&self) -> bool {
        self.interval.is_none()
    }

    /// SQL expression that maps a nanosecond `block_timestamp` column to its bucket's start, in
    /// epoch seconds.
    ///
    /// In the single-span mode there is only one bucket, so it collapses to a constant and the
    /// grouping key stops depending on the row at all.
    pub fn truncated_timestamp(&self, column: &str) -> String {
        match self.interval {
            None => format!("({})::BIGINT", self.start()),
            Some(interval) => format!(
                "EXTRACT(EPOCH FROM date_trunc('{}', to_timestamp({}/1000000000/300*300)))::BIGINT",
                interval.date_trunc_field(),
                column
            ),
        }
    }
}

/// Parsed `from`/`to`/`count`/`interval` query parameters.
#[derive(Debug, Clone, Copy, Default)]
pub struct BucketParams {
    pub from: Option<Second>,
    pub to: Option<Second>,
    pub count: Option<i64>,
    pub interval: Option<Interval>,
}

const USAGE: &str = "\
Usage:

With an interval parameter you get a series of buckets:
- interval: 5min, hour, day, week, month, quarter, year
- count: optional int, 1..400
- from/to: optional int, unix seconds

Possible configurations with interval:
- ?interval=day&count=10                       - last 10 days
- ?interval=day&count=10&to=1608825600         - last 10 days before to
- ?interval=day&count=10&from=1606780800       - next 10 days after from
- ?interval=day&from=1606780800&to=1608825600  - days between from and to

Without interval you get a single span:
- ?from=1606780842&to=1608825642               - meta for this span
- ?from=1606780842                             - until now
- ?to=1608825642                               - since the start of the chain
- no parameters                                - the whole chain";

/// Turn query parameters into bucket boundaries, clamped to the data we actually have.
pub fn generate(params: BucketParams, cursor: &BlockCursor) -> Result<Buckets, Error> {
    let first = cursor.first().timestamp.to_second();
    let now = cursor.now_second();

    let Some(interval) = params.interval else {
        if params.count.is_some() {
            return Err(Error::bad_request(format!(
                "count was provided but no interval parameter.\n{USAGE}"
            )));
        }
        return Ok(Buckets::one(
            params.from.unwrap_or(first),
            params.to.unwrap_or(now),
        ));
    };

    let Some(count) = params.count else {
        let window = Window {
            from: params.from.unwrap_or(first),
            until: params.to.unwrap_or(now),
        };
        let mut buckets = boundaries(interval, window)?;
        if buckets.count() > MAX_INTERVAL_COUNT {
            buckets.timestamps.truncate(MAX_INTERVAL_COUNT + 1);
        }
        restrict(first, now, &mut buckets);
        return Ok(buckets);
    };

    if count < 1 || count > MAX_INTERVAL_COUNT as i64 {
        return Err(Error::bad_request(format!(
            "Count out of range: {count}, allowed [1..{MAX_INTERVAL_COUNT}].\n{USAGE}"
        )));
    }
    if params.from.is_some() && params.to.is_some() {
        return Err(Error::bad_request(format!(
            "Count and from and to was specified. Specify max 2 of them.\n{USAGE}"
        )));
    }

    // Ask for a window wide enough that `count` whole intervals are guaranteed to fit, then trim
    // from the end we did not anchor to.
    let span = count * interval.max_duration();
    let (window, trim_from_start) = match (params.from, params.to) {
        (Some(from), _) => (
            Window {
                from,
                until: from + span,
            },
            false,
        ),
        (None, to) => {
            let to = to.unwrap_or(now);
            (
                Window {
                    from: to - span,
                    until: to,
                },
                true,
            )
        }
    };

    let mut buckets = boundaries(interval, window)?;
    restrict(first, now, &mut buckets);

    let count = count as usize;
    if buckets.count() > count {
        if trim_from_start {
            buckets.timestamps.drain(..buckets.count() - count);
        } else {
            buckets.timestamps.truncate(count + 1);
        }
    }
    Ok(buckets)
}

/// Calendar-aligned boundaries covering `window`.
///
/// The first boundary is at or before `window.from` and the last is strictly after
/// `window.until`, so the window is fully covered at both ends.
fn boundaries(interval: Interval, window: Window) -> Result<Buckets, Error> {
    if window.until <= window.from {
        return Err(Error::bad_request(format!(
            "'from' ({}) must be before 'to' ({}).\n{USAGE}",
            window.from, window.until
        )));
    }
    if interval.max_duration() * CUTOFF_WINDOW_MULTIPLE < (window.until - window.from) {
        return Err(Error::bad_request(format!(
            "Too wide range requested, max allowed intervals ({MAX_INTERVAL_COUNT}).\n{USAGE}"
        )));
    }

    let mut timestamps = Vec::new();
    let mut t = truncate(interval, window.from);
    // Stop at the first boundary at or past `until` and push exactly that one, so the final
    // bucket is closed without adding a spurious empty one. When `until` lands exactly on a
    // boundary this makes it the last timestamp rather than the second to last.
    while t < window.until {
        timestamps.push(t);
        let next = advance(interval, t);
        debug_assert!(next > t, "interval must advance");
        t = next;
        if timestamps.len() > MAX_INTERVAL_COUNT + 2 {
            break;
        }
    }
    timestamps.push(t);

    if timestamps.len() < 2 {
        return Err(Error::bad_request(format!(
            "No interval requested. Use count or a wider from/to range.\n{USAGE}"
        )));
    }
    Ok(Buckets {
        timestamps,
        interval: Some(interval),
    })
}

/// Clamp to the range we hold data for.
///
/// Leaves at most one boundary before the first block and at most one after the last, and never
/// leaves fewer than two, so the caller always has a usable span even when the request is
/// entirely outside the chain's lifetime.
fn restrict(first_block: Second, last_block: Second, buckets: &mut Buckets) {
    let mut last_ok = buckets.timestamps.len() - 1;
    while last_ok > 1 && last_block < buckets.timestamps[last_ok - 1] {
        last_ok -= 1;
    }
    let mut first_ok = 0;
    while first_ok < last_ok - 1 && buckets.timestamps[first_ok + 1] < first_block {
        first_ok += 1;
    }
    buckets.timestamps = buckets.timestamps[first_ok..=last_ok].to_vec();
}

/// Round down to a calendar boundary, matching postgres' `date_trunc`.
fn truncate(interval: Interval, t: Second) -> Second {
    let s = t.to_i64();
    match interval {
        Interval::Min5 => Second(s.div_euclid(300) * 300),
        Interval::Hour => Second(s.div_euclid(3_600) * 3_600),
        Interval::Day => Second(s.div_euclid(86_400) * 86_400),
        // 1970-01-01 was a Thursday and postgres weeks start on Monday, so the epoch sits three
        // days into a week. Shift onto a Monday-aligned axis before truncating.
        Interval::Week => {
            const THURSDAY_OFFSET: i64 = 3 * 86_400;
            let shifted = s + THURSDAY_OFFSET;
            Second(shifted.div_euclid(7 * 86_400) * (7 * 86_400) - THURSDAY_OFFSET)
        }
        Interval::Month | Interval::Quarter | Interval::Year => {
            let (mut year, mut month, _) = civil_from_epoch_seconds(s);
            match interval {
                Interval::Month => {}
                Interval::Quarter => month = (month - 1) / 3 * 3 + 1,
                Interval::Year => month = 1,
                _ => unreachable!(),
            }
            if month < 1 {
                month = 1;
                year -= 1;
            }
            Second(epoch_seconds_from_civil(year, month, 1))
        }
    }
}

/// The start of the interval after the one beginning at `t`.
fn advance(interval: Interval, t: Second) -> Second {
    match interval {
        Interval::Min5 | Interval::Hour | Interval::Day | Interval::Week => {
            t + interval.min_duration()
        }
        Interval::Month | Interval::Quarter | Interval::Year => {
            let (year, month, _) = civil_from_epoch_seconds(t.to_i64());
            let step = match interval {
                Interval::Month => 1,
                Interval::Quarter => 3,
                Interval::Year => 12,
                _ => unreachable!(),
            };
            let total = (year * 12 + (month - 1)) + step;
            Second(epoch_seconds_from_civil(
                total.div_euclid(12),
                total.rem_euclid(12) + 1,
                1,
            ))
        }
    }
}

/// Days-from-epoch to a civil date, after Howard Hinnant's `civil_from_days`.
///
/// Rolled by hand rather than pulling in chrono: this and its inverse are the only calendar
/// arithmetic in the whole daemon, and everything else here is integer seconds.
fn civil_from_epoch_seconds(seconds: i64) -> (i64, i64, i64) {
    let days = seconds.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Inverse of [`civil_from_epoch_seconds`], for midnight UTC on the given date.
fn epoch_seconds_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_log::BlockPos;
    use midgard_core::Nano;

    /// 2021-01-01T00:00:00Z, a Friday.
    const JAN_2021: i64 = 1_609_459_200;

    fn cursor(first: i64, last: i64) -> BlockCursor {
        let c = BlockCursor::new();
        c.observe(BlockPos {
            height: 1,
            timestamp: Second(first).to_nano(),
        });
        c.observe(BlockPos {
            height: 2,
            timestamp: Nano(last * 1_000_000_000),
        });
        c
    }

    #[test]
    fn truncation_matches_calendar_edges() {
        // 2021-03-15T13:47:31Z
        let t = Second(1_615_816_051);
        assert_eq!(truncate(Interval::Min5, t), Second(1_615_815_900));
        assert_eq!(truncate(Interval::Hour, t), Second(1_615_813_200));
        assert_eq!(truncate(Interval::Day, t), Second(1_615_766_400));
        // 2021-03-01
        assert_eq!(truncate(Interval::Month, t), Second(1_614_556_800));
        // 2021-01-01, quarter starting in January
        assert_eq!(truncate(Interval::Quarter, t), Second(JAN_2021));
        assert_eq!(truncate(Interval::Year, t), Second(JAN_2021));
    }

    /// Reference values taken from postgres itself:
    ///
    /// ```sql
    /// SET timezone='UTC';
    /// SELECT EXTRACT(EPOCH FROM date_trunc('week', to_timestamp(s)))::BIGINT ...
    /// ```
    ///
    /// This is the test that matters most in the module. Bucket boundaries are computed here in
    /// Rust but the `GROUP BY` in every history query uses postgres' `date_trunc`, so if the two
    /// disagree by even one second, rows land in a bucket whose declared range does not contain
    /// them and the totals are quietly wrong.
    #[test]
    fn truncation_agrees_with_postgres_date_trunc() {
        // s, 5min, hour, day, week, month, quarter, year
        const CASES: &[[i64; 8]] = &[
            [1, 0, 0, 0, -259_200, 0, 0, 0],
            [
                946_684_800,
                946_684_800,
                946_684_800,
                946_684_800,
                946_252_800,
                946_684_800,
                946_684_800,
                946_684_800,
            ],
            [
                1_583_020_800,
                1_583_020_800,
                1_583_020_800,
                1_583_020_800,
                1_582_502_400,
                1_583_020_800,
                1_577_836_800,
                1_577_836_800,
            ],
            [
                1_609_459_200,
                1_609_459_200,
                1_609_459_200,
                1_609_459_200,
                1_609_113_600,
                1_609_459_200,
                1_609_459_200,
                1_609_459_200,
            ],
            [
                1_615_816_051,
                1_615_815_900,
                1_615_813_200,
                1_615_766_400,
                1_615_766_400,
                1_614_556_800,
                1_609_459_200,
                1_609_459_200,
            ],
            [
                1_735_689_599,
                1_735_689_300,
                1_735_686_000,
                1_735_603_200,
                1_735_516_800,
                1_733_011_200,
                1_727_740_800,
                1_704_067_200,
            ],
            [
                2_145_916_800,
                2_145_916_800,
                2_145_916_800,
                2_145_916_800,
                2_145_571_200,
                2_145_916_800,
                2_145_916_800,
                2_145_916_800,
            ],
        ];

        const INTERVALS: [Interval; 7] = [
            Interval::Min5,
            Interval::Hour,
            Interval::Day,
            Interval::Week,
            Interval::Month,
            Interval::Quarter,
            Interval::Year,
        ];

        for case in CASES {
            let s = Second(case[0]);
            for (i, interval) in INTERVALS.iter().enumerate() {
                assert_eq!(
                    truncate(*interval, s),
                    Second(case[i + 1]),
                    "{} at {}",
                    interval.name(),
                    case[0]
                );
            }
        }
    }

    #[test]
    fn weeks_start_on_monday() {
        // 2021-03-15 is itself a Monday, so it is its own week start.
        assert_eq!(
            truncate(Interval::Week, Second(1_615_816_051)),
            Second(1_615_766_400)
        );
        // 2021-03-14 is the Sunday before, and belongs to the previous week.
        assert_eq!(
            truncate(Interval::Week, Second(1_615_730_000)),
            Second(1_615_161_600)
        );
    }

    #[test]
    fn months_are_not_fixed_length() {
        // Jan (31d) then Feb (28d in 2021).
        let jan = Second(JAN_2021);
        let feb = advance(Interval::Month, jan);
        let mar = advance(Interval::Month, feb);
        assert_eq!(feb - jan, 31 * 86_400);
        assert_eq!(mar - feb, 28 * 86_400);
    }

    #[test]
    fn quarters_and_years_advance_by_whole_months() {
        let q1 = Second(JAN_2021);
        let q2 = advance(Interval::Quarter, q1);
        // Jan + Feb + Mar 2021 = 31 + 28 + 31 days.
        assert_eq!(q2 - q1, 90 * 86_400);
        assert_eq!(advance(Interval::Year, q1) - q1, 365 * 86_400);
    }

    #[test]
    fn civil_conversions_round_trip() {
        for s in [0, JAN_2021, 1_615_816_051, 253_370_764_800] {
            let (y, m, d) = civil_from_epoch_seconds(s);
            let back = epoch_seconds_from_civil(y, m, d);
            assert_eq!(back, s.div_euclid(86_400) * 86_400, "{s} -> {y}-{m}-{d}");
        }
    }

    #[test]
    fn no_interval_gives_a_single_span() {
        let c = cursor(JAN_2021, JAN_2021 + 86_400);
        let b = generate(BucketParams::default(), &c).unwrap();
        assert!(b.is_one_interval());
        assert_eq!(b.count(), 1);
        assert_eq!(b.start(), Second(JAN_2021));
    }

    #[test]
    fn count_and_interval_produce_that_many_buckets() {
        let c = cursor(JAN_2021 - 400 * 86_400, JAN_2021);
        let b = generate(
            BucketParams {
                interval: Some(Interval::Day),
                count: Some(10),
                ..Default::default()
            },
            &c,
        )
        .unwrap();
        assert_eq!(b.count(), 10);
        assert_eq!(b.timestamps().len(), 11);
        // Every boundary is a day edge, and they are strictly increasing.
        for w in b.timestamps().windows(2) {
            assert_eq!(w[0].to_i64() % 86_400, 0);
            assert_eq!(w[1] - w[0], 86_400);
        }
    }

    #[test]
    fn from_and_count_anchors_at_the_start() {
        let c = cursor(JAN_2021 - 86_400, JAN_2021 + 100 * 86_400);
        let b = generate(
            BucketParams {
                from: Some(Second(JAN_2021)),
                count: Some(3),
                interval: Some(Interval::Day),
                ..Default::default()
            },
            &c,
        )
        .unwrap();
        assert_eq!(b.count(), 3);
        assert_eq!(b.start(), Second(JAN_2021));
        assert_eq!(b.end(), Second(JAN_2021 + 3 * 86_400));
    }

    #[test]
    fn to_and_count_anchors_at_the_end() {
        let c = cursor(JAN_2021 - 100 * 86_400, JAN_2021 + 86_400);
        let b = generate(
            BucketParams {
                to: Some(Second(JAN_2021)),
                count: Some(3),
                interval: Some(Interval::Day),
                ..Default::default()
            },
            &c,
        )
        .unwrap();
        assert_eq!(b.count(), 3);
        assert_eq!(b.end(), Second(JAN_2021));
        assert_eq!(b.start(), Second(JAN_2021 - 3 * 86_400));
    }

    #[test]
    fn buckets_are_clamped_to_available_data() {
        // Chain only has one day of data but the request asks for a year.
        let c = cursor(JAN_2021, JAN_2021 + 86_400);
        let b = generate(
            BucketParams {
                from: Some(Second(JAN_2021 - 300 * 86_400)),
                to: Some(Second(JAN_2021 + 86_400)),
                interval: Some(Interval::Day),
                ..Default::default()
            },
            &c,
        )
        .unwrap();
        assert!(b.count() <= 3, "clamped to {} buckets", b.count());
        assert!(b.end() >= Second(JAN_2021));
    }

    #[test]
    fn rejects_count_without_interval() {
        let c = cursor(JAN_2021, JAN_2021 + 86_400);
        let err = generate(
            BucketParams {
                count: Some(5),
                ..Default::default()
            },
            &c,
        )
        .unwrap_err();
        assert_eq!(err.status_code(), 400);
    }

    #[test]
    fn rejects_count_with_both_ends() {
        let c = cursor(JAN_2021, JAN_2021 + 86_400);
        let err = generate(
            BucketParams {
                from: Some(Second(JAN_2021)),
                to: Some(Second(JAN_2021 + 86_400)),
                count: Some(5),
                interval: Some(Interval::Day),
            },
            &c,
        )
        .unwrap_err();
        assert_eq!(err.status_code(), 400);
    }

    #[test]
    fn rejects_out_of_range_count() {
        let c = cursor(JAN_2021, JAN_2021 + 86_400);
        for count in [0, -1, MAX_INTERVAL_COUNT as i64 + 1] {
            let err = generate(
                BucketParams {
                    count: Some(count),
                    interval: Some(Interval::Day),
                    ..Default::default()
                },
                &c,
            )
            .unwrap_err();
            assert_eq!(err.status_code(), 400, "count={count}");
        }
    }

    #[test]
    fn rejects_an_absurdly_wide_range() {
        let c = cursor(0, 4_000_000_000);
        let err = generate(
            BucketParams {
                from: Some(Second(0)),
                to: Some(Second(4_000_000_000)),
                interval: Some(Interval::Min5),
                ..Default::default()
            },
            &c,
        )
        .unwrap_err();
        assert_eq!(err.status_code(), 400);
    }

    #[test]
    fn interval_names_round_trip() {
        for i in [
            Interval::Min5,
            Interval::Hour,
            Interval::Day,
            Interval::Week,
            Interval::Month,
            Interval::Quarter,
            Interval::Year,
        ] {
            assert_eq!(Interval::parse(i.name()), Some(i));
        }
        assert_eq!(Interval::parse("fortnight"), None);
        assert_eq!(Interval::parse("DAY"), Some(Interval::Day));
    }

    #[test]
    fn single_span_grouping_key_is_constant() {
        let b = Buckets::one(Second(100), Second(200));
        assert_eq!(b.truncated_timestamp("block_timestamp"), "(100)::BIGINT");
    }
}
