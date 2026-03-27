//! Turning blocks into rows.
//!
//! The pipeline is: a [`midgard_chain::Block`] goes in, its ABCI events are decoded
//! ([`events`]), the pool depths implied by them are tracked ([`depth`]), and the result is
//! buffered and written in one transaction per batch ([`writer`]).
//!
//! Two invariants hold this together, and both are about crash safety:
//!
//! * a block's rows and its `block_log` entry are written in the *same* transaction, so the
//!   cursor can never point past data that is not there;
//! * decoding is a pure function of the block, so replaying a block produces byte-identical
//!   rows and re-syncing is always a valid recovery.

pub mod coin;
pub mod depth;
pub mod events;
pub mod writer;

pub use coin::{parse_coin, parse_coins, Coin};
pub use events::{decode, DecodeError, Decoded, Direction, Recorded};
pub use writer::{BlockWriter, WriteError};
