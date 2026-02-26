//! Domain types shared by every other midgard crate.
//!
//! Nothing in here talks to the database, the chain, or the network. It is the vocabulary the
//! rest of the daemon is written in: what an asset is, how amounts are scaled, and how block
//! time is represented.

pub mod asset;
pub mod error;

pub use asset::{Asset, CoinType};
pub use error::{Error, Result};
