//! Event identifiers.
//!
//! Every recorded event gets a single `BIGINT` that sorts in chain order. Sorting is the whole
//! point: the actions feed and every "what happened after X" query become a plain `ORDER BY
//! event_id` with no tuple comparison and no composite index.
//!
//! The encoding is decimal so that a human staring at a value in psql can read the height
//! straight off the front of it:
//!
//! ```text
//! begin block   h,hhh,hhh,hh0,eee,eee,eee
//! tx results    h,hhh,hhh,hh[1-8],ttt,tte,eee
//! end block     h,hhh,hhh,hh9,eee,eee,eee
//! ```
//!
//! `h` is the height, `t` the transaction index, `e` the event index. The digit after the height
//! is what orders the three phases of a block relative to each other, and it is why begin-block
//! uses 0 and end-block uses 9 rather than 0/1/2 — it leaves room in the middle for the eight
//! hundred thousand transactions a block may carry.
//!
//! The budget works out to 922 million blocks (about 146 years at 5s blocks — `i64::MAX / 1e10`
//! is the binding constraint, not the digit layout), a billion events in each of the begin and
//! end phases, 800k transactions per block, and 10k events per transaction.

use std::fmt;

/// Which phase of block execution an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Location {
    BeginBlock,
    TxsResults,
    EndBlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventId {
    pub height: i64,
    pub location: Location,
    pub tx_index: i64,
    pub event_index: i64,
}

const HEIGHT_SCALE: i64 = 10_000_000_000; // 1e10
const TYPE_DIGIT: i64 = 1_000_000_000; // 1e9
const TX_INDEX_SCALE: i64 = 10_000; // 1e4

/// The tx index reported for an end-block event, which has no transaction of its own.
pub const END_BLOCK_PSEUDO_TX: i64 = 999_999;

impl EventId {
    /// Start of a block's begin-block phase. Event indices are 1-based, matching the Go
    /// implementation, so that 0 can mean "the block itself".
    pub fn begin_block(height: i64) -> EventId {
        EventId {
            height,
            location: Location::BeginBlock,
            tx_index: 0,
            event_index: 1,
        }
    }

    pub fn tx_event(height: i64, tx_index: i64, event_index: i64) -> EventId {
        EventId {
            height,
            location: Location::TxsResults,
            tx_index,
            event_index,
        }
    }

    pub fn end_block(height: i64, event_index: i64) -> EventId {
        EventId {
            height,
            location: Location::EndBlock,
            tx_index: 0,
            event_index,
        }
    }

    pub fn to_i64(self) -> i64 {
        match self.location {
            Location::BeginBlock => self.height * HEIGHT_SCALE + self.event_index,
            Location::TxsResults => {
                self.height * HEIGHT_SCALE
                    + TYPE_DIGIT
                    + self.tx_index * TX_INDEX_SCALE
                    + self.event_index
            }
            Location::EndBlock => self.height * HEIGHT_SCALE + 9 * TYPE_DIGIT + self.event_index,
        }
    }

    pub fn parse(encoded: i64) -> EventId {
        let height = encoded / HEIGHT_SCALE;
        let rest = encoded % HEIGHT_SCALE;
        if rest < TYPE_DIGIT {
            EventId {
                height,
                location: Location::BeginBlock,
                tx_index: 0,
                event_index: rest,
            }
        } else if rest < 9 * TYPE_DIGIT {
            EventId {
                height,
                location: Location::TxsResults,
                tx_index: (rest - TYPE_DIGIT) / TX_INDEX_SCALE,
                event_index: rest % TX_INDEX_SCALE,
            }
        } else {
            EventId {
                height,
                location: Location::EndBlock,
                tx_index: END_BLOCK_PSEUDO_TX,
                event_index: rest % TYPE_DIGIT,
            }
        }
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_i64())
    }
}

/// The height an event id belongs to.
pub fn height_of(event_id: i64) -> i64 {
    event_id / HEIGHT_SCALE
}

/// The lowest event id that can occur at `height`. Use as an inclusive lower bound.
pub fn first_id_at_height(height: i64) -> i64 {
    height * HEIGHT_SCALE
}

/// The lowest event id strictly after every event at `height`. Use as an exclusive upper bound.
pub fn first_id_after_height(height: i64) -> i64 {
    (height + 1) * HEIGHT_SCALE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_sort_in_execution_order() {
        let begin = EventId::begin_block(100).to_i64();
        let tx = EventId::tx_event(100, 1, 1).to_i64();
        let end = EventId::end_block(100, 1).to_i64();
        assert!(begin < tx, "{begin} < {tx}");
        assert!(tx < end, "{tx} < {end}");
    }

    #[test]
    fn heights_sort_before_phases() {
        assert!(EventId::end_block(100, 999).to_i64() < EventId::begin_block(101).to_i64());
    }

    #[test]
    fn round_trips() {
        let cases = [
            EventId::begin_block(1),
            EventId::begin_block(27_260_627),
            EventId::tx_event(27_260_627, 0, 1),
            EventId::tx_event(27_260_627, 12, 340),
            EventId::tx_event(1, 799_999, 9_999),
            EventId::end_block(27_260_627, 42),
        ];
        for id in cases {
            let parsed = EventId::parse(id.to_i64());
            assert_eq!(parsed.height, id.height, "{id:?}");
            assert_eq!(parsed.location, id.location, "{id:?}");
            assert_eq!(parsed.event_index, id.event_index, "{id:?}");
            if id.location == Location::TxsResults {
                assert_eq!(parsed.tx_index, id.tx_index, "{id:?}");
            }
        }
    }

    #[test]
    fn end_block_reports_the_pseudo_tx_index() {
        let parsed = EventId::parse(EventId::end_block(500, 3).to_i64());
        assert_eq!(parsed.tx_index, END_BLOCK_PSEUDO_TX);
    }

    #[test]
    fn height_bounds_bracket_every_event_in_a_block() {
        let h = 27_260_627;
        let lo = first_id_at_height(h);
        let hi = first_id_after_height(h);
        for id in [
            EventId::begin_block(h),
            EventId::tx_event(h, 799_999, 9_999),
            EventId::end_block(h, 999_999_999),
        ] {
            let v = id.to_i64();
            assert!(lo <= v && v < hi, "{v} outside [{lo}, {hi})");
        }
        assert_eq!(height_of(lo), h);
        assert_eq!(height_of(hi - 1), h);
        assert_eq!(height_of(hi), h + 1);
    }

    /// The ceiling is `i64::MAX / HEIGHT_SCALE`, which is what the module doc quotes. Worth
    /// pinning down: the decimal layout suggests a billion blocks, but the type gives out first.
    #[test]
    fn stays_inside_i64_at_the_documented_limits() {
        let max_height = i64::MAX / HEIGHT_SCALE;
        assert_eq!(max_height, 922_337_203);

        // One below the ceiling, fully loaded, still fits.
        let id = EventId::end_block(max_height - 1, 999_999_999);
        assert!(id.to_i64() > 0, "overflowed to {}", id.to_i64());
        assert_eq!(EventId::parse(id.to_i64()).height, max_height - 1);

        // 146 years of 5s blocks is the practical reading of that number.
        assert!(max_height * 5 / (365 * 24 * 60 * 60) >= 145);
    }
}
