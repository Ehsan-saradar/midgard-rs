//! Talking to THORChain.
//!
//! Two very different sources feed Midgard:
//!
//! * the **Tendermint RPC**, which is the authoritative one. Every pool depth, swap and deposit
//!   is reconstructed from the ABCI events in `block_results`, and replaying those from genesis
//!   reproduces the database exactly.
//! * the **THORNode REST API**, for the handful of things the event stream does not carry —
//!   node bonds, mimir values, the current pool set. See [`thornode`]. That data is a snapshot
//!   of the tip and cannot be reconstructed historically, which is why the endpoints that depend
//!   on it are the ones with no history variant.

pub mod client;
pub mod rpc;
pub mod thornode;
pub mod types;

pub use client::{BlockIterator, ChainError, Client};
pub use rpc::{RpcClient, RpcError};
pub use types::{attr, attrs, Block, BlockResults, Status};
