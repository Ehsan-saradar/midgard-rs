# midgard-rs

A Rust implementation of [Midgard](https://gitlab.com/thorchain/midgard), the layer-2 REST API
that serves rolled-up analytics for the THORChain network.

Midgard reads blocks from a THORNode's Tendermint RPC endpoint, decodes the ABCI events emitted
by `thorchain`, writes them into a TimescaleDB hypertable per event type, and serves aggregated
views of that data over HTTP. It exists so that front-ends and indexers hammer a read replica
instead of the chain itself.

This port targets the same `/v2` wire format as the Go implementation, so existing clients should
not be able to tell the two apart for the endpoints that are implemented.

## Status

The block pipeline (fetch → decode → record → aggregate) is complete, and these endpoints are
served:

| Endpoint | Notes |
| --- | --- |
| `GET /v2/health` | sync state, last committed height |
| `GET /v2/pools` | pool list with depths, price, APR |
| `GET /v2/pool/{asset}` | single pool detail |
| `GET /v2/knownpools` | asset → status map |
| `GET /v2/history/depths/{pool}` | depth, price and LUVI per bucket |
| `GET /v2/history/swaps` | volume, fees and slip, split by direction |
| `GET /v2/history/earnings` | system income, split per pool |
| `GET /v2/history/liquidity_changes` | deposit and withdrawal volume |
| `GET /v2/history/tvl` | total value locked |
| `GET /v2/actions` | paginated action feed |
| `GET /v2/members`, `GET /v2/member/{addr}` | liquidity provider positions |
| `GET /v2/network`, `GET /v2/stats` | network-wide counters |
| `GET /v2/debug/metrics` | Prometheus gauges, including `midgard_blocks_behind` |

Not yet covered, and served by upstream: savers, borrowers, RUNEPool, THORNames, votes, TCY,
affiliate history, the websocket feed, and the `/v2/thorchain/*` proxy.

Some fields inside implemented endpoints are reported as zero rather than guessed, because the
data behind them is THORNode state rather than anything in the event stream — node counts and
bonded value, most visibly. These are called out where they occur.

## Running

```sh
docker compose up -d pg
cargo run --bin midgard -- config/base.json:config/pg.json:config/net-main.json
```

Note that THORChain serves Tendermint RPC on **27147**, not CometBFT's default 26657.

Configuration is a colon-separated list of JSON files, merged left to right, so each file only
carries the keys it changes. Any value can also be overridden with a `MIDGARD_`-prefixed
environment variable, where nesting is expressed with underscores:

```sh
MIDGARD_LISTEN_PORT=9000 MIDGARD_TIMESCALE_HOST=pg cargo run --bin midgard -- config/base.json
```

### Starting part-way along the chain

Depths are reconstructed by replaying deltas, so they are only absolute if the replay started at
block 1. Starting mid-chain against an empty database would leave every pool at zero and
promptly negative, so the daemon instead seeds the opening depths from THORNode:

```sh
MIDGARD_GENESIS_INITIAL_BLOCK_HEIGHT=27260000 cargo run --bin midgard -- config/base.json:config/pg.json:config/net-main.json
```

A seeded database is **correct from that height onwards and has no history before it**. That is
usually what you want for a node following the tip, and never what you want for one backing
historical charts — for those, sync from block 1.

## Testing

Unit tests need nothing:

```sh
cargo test
```

The integration tests are skipped unless pointed at a real database and node. They drop and
rebuild the schema, so use a throwaway database:

```sh
docker compose up -d pg
MIDGARD_TEST_DB=postgres://midgard:password@localhost:5432/midgard \
MIDGARD_TEST_TENDERMINT=http://localhost:27147 \
  cargo test
```

## Layout

```
crates/
  midgard-core     assets, fixed-point amounts, block time, shared error type
  midgard-config   config schema, file merging, env overlay
  midgard-db       connection pool, DDL, schema versioning, time bucketing
  midgard-chain    Tendermint JSON-RPC client, THORNode REST client, block iterator
  midgard-record   ABCI event decoding, batch writer, depth tracking
  midgard-api      axum router and the /v2 handlers
  midgard          the daemon
```

## Notes on the port

A few decisions differ from the Go implementation, deliberately:

- **Wire types come from `tendermint-rpc`, the transport does not.** That crate tracks CometBFT's
  schema across versions, which is real ongoing work worth not repeating. Its HTTP client has no
  JSON-RPC batching though, and catching up is two calls times twenty-seven million blocks, so
  `midgard-chain::rpc` provides that. Pulling the crate in with `default-features = false` skips
  its client stack and takes the dependency tree from 247 crates to 164.

- **No migrations.** The database holds nothing that cannot be derived by replaying the chain, so
  a schema mismatch drops and rebuilds rather than migrating. The stored fingerprint is a hash of
  the DDL text, not a version integer someone has to remember to bump. `no_auto_update_ddl` turns
  the rebuild into a hard failure for operators who would rather the deploy break loudly.

- **Bucket boundaries are computed in Rust.** Upstream borrows postgres' `date_trunc` via a
  `time_bucket_gapfill` query with `WHERE 1=0`, which costs a round trip before the real query.
  The semantics are reproduced exactly instead, and the test that matters compares against values
  taken straight out of psql for all seven intervals — the `GROUP BY` still uses `date_trunc`, so
  a one-second disagreement would put rows in a bucket whose declared range excludes them.

- **Aggregates are computed on read.** Upstream materialises TimescaleDB continuous aggregates.
  Queries here hit the event tables directly: simpler and always consistent, but slower on wide
  ranges over a fully-synced database. `/v2/stats`, which scans everything since genesis, is
  cached against the committed block height — an answer computed at height N is exactly right
  until N+1, so there is no staleness window to tune.

## Verifying against a node

With a THORNode reachable, the quickest check that the pipeline is correct is to compare pool
depths against the node's own view:

```sh
curl -s localhost:8080/v2/pools | jq -r '.[] | "\(.asset) \(.runeDepth)"' | head
curl -s localhost:1317/thorchain/pools | jq -r '.[] | "\(.asset) \(.balance_rune)"' | head
```

They should agree to within the few blocks of sync lag. `midgard_blocks_behind` from
`/v2/debug/metrics` tells you how much lag that is.

## License

MIT, same as upstream Midgard. See [LICENSE](LICENSE).
