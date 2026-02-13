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

The block pipeline (fetch → decode → record → aggregate) is complete, and the following endpoints
are served:

| Endpoint | Notes |
| --- | --- |
| `GET /v2/health` | sync state, last fetched/committed heights |
| `GET /v2/pools` | pool list with depths, price, APR |
| `GET /v2/pool/{asset}` | single pool detail |
| `GET /v2/knownpools` | asset → status map |
| `GET /v2/history/depths/{pool}` | depth/price buckets with OHLC |
| `GET /v2/history/swaps` | swap volume/fee/slip buckets |
| `GET /v2/history/earnings` | system income split per pool |
| `GET /v2/history/liquidity_changes` | add/withdraw volume buckets |
| `GET /v2/history/tvl` | total value locked buckets |
| `GET /v2/actions` | paginated action feed |
| `GET /v2/members`, `GET /v2/member/{addr}` | liquidity provider positions |
| `GET /v2/network`, `GET /v2/stats` | network-wide counters |
| `GET /v2/debug/metrics` | Prometheus exposition |

Endpoints that the Go implementation serves but this port does not yet cover: savers, borrowers,
RUNEPool, THORNames, votes, TCY, affiliate history, the websocket feed, and the `/v2/thorchain/*`
proxy.

## Running

```sh
docker compose up -d pg
cargo run --bin midgard -- config/base.json:config/pg.json:config/net-main.json
```

Configuration is a colon-separated list of JSON files; later files override earlier ones. Any
value can also be overridden with a `MIDGARD_`-prefixed environment variable, where nesting is
expressed with underscores:

```sh
MIDGARD_LISTEN_PORT=9000 MIDGARD_TIMESCALE_HOST=pg cargo run --bin midgard -- config/base.json
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

## License

MIT, same as upstream Midgard. See [LICENSE](LICENSE).
