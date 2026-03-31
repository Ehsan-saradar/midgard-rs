//! Tests against a real THORNode.
//!
//! Skipped unless `MIDGARD_TEST_TENDERMINT` points at one:
//!
//! ```sh
//! MIDGARD_TEST_TENDERMINT=http://localhost:27147 cargo test -p midgard-chain
//! ```
//!
//! Read-only, so any node will do. The point is to catch the failures unit tests structurally
//! cannot: a batched response reassembled in the wrong order, a schema change in a CometBFT
//! upgrade, a height that comes back describing a different block.

use midgard_chain::{attr, Client};
use midgard_config::ThorChain;

fn config() -> Option<ThorChain> {
    let url = std::env::var("MIDGARD_TEST_TENDERMINT").ok()?;
    Some(ThorChain {
        tendermint_url: url,
        ..ThorChain::default()
    })
}

macro_rules! client_or_skip {
    ($batch:expr, $par:expr) => {
        match config() {
            Some(mut cfg) => {
                cfg.fetch_batch_size = $batch;
                cfg.parallelism = $par;
                Client::new(&cfg).expect("client")
            }
            None => {
                eprintln!("MIDGARD_TEST_TENDERMINT not set, skipping");
                return;
            }
        }
    };
}

/// A height comfortably inside any mainnet node's retained window, relative to the tip.
async fn sample_height(client: &Client) -> i64 {
    client.latest_height().await.expect("status") - 200
}

#[tokio::test]
async fn status_reports_a_tip() {
    let client = client_or_skip!(1, 1);
    let status = client.status().await.unwrap();
    assert!(status.sync_info.latest_block_height.value() > 0);
    assert!(!status.node_info.network.to_string().is_empty());
}

#[tokio::test]
async fn a_single_block_round_trips() {
    let client = client_or_skip!(1, 1);
    let height = sample_height(&client).await;

    let block = client.fetch_block(height).await.unwrap();
    assert_eq!(block.height, height);
    assert_eq!(block.hash.len(), 64, "hash should be 32 bytes of hex");
    assert!(block.hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(block.timestamp.to_i64() > 0);
    assert!(!block.chain_id.is_empty());

    // Every block emits *something*. Which phase those events land in is deliberately not
    // asserted: on the CometBFT 0.38 stream THORNode produces, effectively all of them carry
    // mode=EndBlock and BeginBlock shows up in roughly one block in ten. An earlier version of
    // this test required begin-block events and failed on whichever heights happened to be
    // sampled.
    assert!(
        !block.results.finalize_block_events.is_empty(),
        "no events at all at height {height}"
    );
}

#[tokio::test]
async fn every_event_phase_is_accounted_for() {
    let client = client_or_skip!(1, 1);
    let height = sample_height(&client).await;
    let block = client.fetch_block(height).await.unwrap();

    let begin = block.begin_block_events().count();
    let end = block.end_block_events().count();
    let total = block.results.finalize_block_events.len();

    // The two phases must partition the list exactly: an event that is in neither would be
    // silently dropped by the recorder, and one in both would be recorded twice.
    assert_eq!(
        begin + end,
        total,
        "begin {begin} + end {end} != total {total}"
    );
}

#[tokio::test]
async fn a_batched_range_comes_back_in_order() {
    let client = client_or_skip!(10, 1);
    let from = sample_height(&client).await;

    let blocks = client.fetch_range(from, 10).await.unwrap();
    assert_eq!(blocks.len(), 10);
    for (i, b) in blocks.iter().enumerate() {
        assert_eq!(b.height, from + i as i64, "block {i} out of order");
    }

    // Timestamps advance monotonically, which is the independent check that the reassembly by
    // JSON-RPC id actually worked rather than just producing plausible heights.
    for w in blocks.windows(2) {
        assert!(
            w[1].timestamp > w[0].timestamp,
            "time went backwards between {} and {}",
            w[0].height,
            w[1].height
        );
    }
}

#[tokio::test]
async fn batched_and_unbatched_agree() {
    let client_batched = client_or_skip!(4, 1);
    let client_single = client_or_skip!(1, 1);
    let from = sample_height(&client_single).await;

    let batched = client_batched.fetch_range(from, 4).await.unwrap();
    for (i, b) in batched.iter().enumerate() {
        let single = client_single.fetch_block(from + i as i64).await.unwrap();
        assert_eq!(b.hash, single.hash);
        assert_eq!(b.timestamp, single.timestamp);
        assert_eq!(
            b.results.finalize_block_events.len(),
            single.results.finalize_block_events.len()
        );
    }
}

#[tokio::test]
async fn parallel_fetching_agrees_with_serial() {
    let parallel = client_or_skip!(8, 4);
    let serial = client_or_skip!(8, 1);
    let from = sample_height(&serial).await;

    let a = parallel.fetch_range(from, 8).await.unwrap();
    let b = serial.fetch_range(from, 8).await.unwrap();

    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x.height, y.height);
        assert_eq!(x.hash, y.hash);
    }
}

#[tokio::test]
async fn the_iterator_walks_a_range_exactly_once() {
    let client = client_or_skip!(3, 1);
    let from = sample_height(&client).await;
    let through = from + 6;

    let mut iter = client.iterator(from, through);
    let mut seen = Vec::new();
    while let Some(block) = iter.next().await.unwrap() {
        seen.push(block.height);
    }

    assert_eq!(seen, (from..=through).collect::<Vec<_>>());
}

#[tokio::test]
async fn asking_for_a_future_height_is_an_error_not_a_hang() {
    let client = client_or_skip!(1, 1);
    let future = client.latest_height().await.unwrap() + 1_000_000;
    assert!(client.fetch_block(future).await.is_err());
}

#[tokio::test]
async fn attributes_are_readable_on_real_events() {
    let client = client_or_skip!(1, 1);
    let height = sample_height(&client).await;
    let block = client.fetch_block(height).await.unwrap();

    // Every end-block event carries mode=EndBlock; that is how we classified it in the first
    // place, so it is a check that attribute reading survives the round trip intact.
    for (_, event) in block.end_block_events() {
        assert_eq!(attr(event, "mode"), Some("EndBlock"), "on {}", event.kind);
    }
}
