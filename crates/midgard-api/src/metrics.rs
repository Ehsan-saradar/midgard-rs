//! `GET /v2/debug/metrics` — Prometheus exposition.
//!
//! Upstream serves this and operators alert on it. The two that matter are
//! `midgard_chain_cursor_height` and `midgard_chain_height`: the gap between them is how far
//! behind the chain we are, which is the number you page on. Everything else is context for
//! working out why.
//!
//! Written by hand rather than pulling in a metrics crate. It is a dozen counters formatted as
//! text, and a registry with its own macros would be more machinery than the thing it holds.

use std::fmt::Write;
use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::response::{IntoResponse, Response};

use crate::AppState;

/// Prometheus text format, version 0.0.4.
const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

pub async fn metrics(State(state): State<AppState>) -> Response {
    let mut out = String::with_capacity(1024);

    let last = state.cursor.last();
    let first = state.cursor.first();
    let chain_height = state.chain_height.load(Ordering::Relaxed);
    let database_up = state.db.ping().await;

    gauge(
        &mut out,
        "midgard_chain_cursor_height",
        "Height of the newest block committed to the database.",
        last.height,
    );
    gauge(
        &mut out,
        "midgard_chain_height",
        "Height of the chain tip as last reported by the node.",
        chain_height,
    );
    // Derived rather than left to the caller: the whole point of the two above is their
    // difference, and PromQL subtraction across scrapes of different ages is subtly wrong.
    gauge(
        &mut out,
        "midgard_blocks_behind",
        "Blocks between the committed height and the chain tip.",
        (chain_height - last.height).max(0),
    );
    gauge(
        &mut out,
        "midgard_earliest_block_height",
        "Height of the oldest block in the database.",
        first.height,
    );
    gauge(
        &mut out,
        "midgard_last_block_timestamp_seconds",
        "Block time of the newest committed block.",
        last.timestamp.to_second().to_i64(),
    );
    gauge(
        &mut out,
        "midgard_database_up",
        "1 if the database answered a ping, 0 otherwise.",
        i64::from(database_up),
    );

    ([(axum::http::header::CONTENT_TYPE, CONTENT_TYPE)], out).into_response()
}

fn gauge(out: &mut String, name: &str, help: &str, value: i64) {
    // write! to a String cannot fail; the Results are discarded rather than unwrapped so a
    // metrics scrape can never be the thing that panics the process.
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {value}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gauge_renders_all_three_lines() {
        let mut s = String::new();
        gauge(&mut s, "midgard_test", "A test gauge.", 42);
        assert_eq!(
            s,
            "# HELP midgard_test A test gauge.\n# TYPE midgard_test gauge\nmidgard_test 42\n"
        );
    }

    #[test]
    fn negative_values_render() {
        // Not expected for these gauges, but the format allows it and silently mangling a
        // negative would be worse than showing one.
        let mut s = String::new();
        gauge(&mut s, "g", "h", -1);
        assert!(s.ends_with("g -1\n"), "{s}");
    }

    /// The number operators actually alert on.
    fn blocks_behind(committed: i64, tip: i64) -> i64 {
        (tip - committed).max(0)
    }

    #[test]
    fn blocks_behind_is_the_gap() {
        assert_eq!(blocks_behind(100, 150), 50);
        assert_eq!(blocks_behind(150, 150), 0);
    }

    #[test]
    fn blocks_behind_never_goes_negative() {
        // The committed height can briefly exceed the last tip we recorded, because the two are
        // sampled at different moments. A negative gap would break a "> 100" alert by wrapping
        // the axis rather than firing.
        assert_eq!(blocks_behind(151, 150), 0);
    }
}
