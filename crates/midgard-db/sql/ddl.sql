-- Midgard schema.
--
-- Bump SCHEMA_VERSION in ddl.rs whenever anything in here changes. On a mismatch the daemon
-- drops the schema and re-syncs from block 1 rather than trying to migrate: the database is a
-- pure projection of the chain, so a rebuild is always available and always correct, and
-- hand-written migrations for a table that is regenerable would be a liability.
--
-- Every event table follows the same shape:
--   * columns mirroring the ABCI event's attributes, amounts as e8 BIGINTs
--   * event_id        a sortable identifier, see eventid.rs
--   * block_timestamp nanoseconds, the hypertable's time dimension
-- Columns prefixed with an underscore are derived by Midgard rather than present in the event.

CREATE EXTENSION IF NOT EXISTS timescaledb CASCADE;

DROP SCHEMA IF EXISTS midgard CASCADE;
CREATE SCHEMA midgard;

-- Catch the case where the search_path is not what we think it is: everything below would
-- otherwise be created somewhere surprising and the daemon would look like it had lost its data.
DO $$ BEGIN
    ASSERT (SELECT current_schema()) = 'midgard', 'current_schema() is not midgard';
END $$;

----------
-- Helpers

-- TimescaleDB needs an integer_now function for hypertables with an integer time dimension.
-- We refresh aggregates explicitly rather than using its background policies, so this only
-- exists to satisfy that requirement.
CREATE FUNCTION current_nano() RETURNS BIGINT
LANGUAGE SQL STABLE AS $$
    SELECT CAST(1000000000 * EXTRACT(EPOCH FROM CURRENT_TIMESTAMP) AS BIGINT)
$$;

CREATE PROCEDURE setup_hypertable(t regclass)
LANGUAGE SQL
AS $$
    SELECT create_hypertable(t, 'block_timestamp',
        chunk_time_interval => (40 * 24 * 60 * 60 * 1000000000 :: BIGINT));
    SELECT set_integer_now_func(t, 'current_nano');
$$;

-- date_trunc over nanoseconds-from-epoch instead of a timestamptz.
CREATE FUNCTION nano_trunc(field TEXT, ts BIGINT) RETURNS BIGINT
LANGUAGE SQL IMMUTABLE AS $$
    SELECT CAST(1000000000 * EXTRACT(EPOCH FROM date_trunc(field, to_timestamp(ts / 1000000000))) AS BIGINT)
$$;

CREATE FUNCTION nano_ts(t BIGINT) RETURNS timestamptz
LANGUAGE SQL IMMUTABLE AS $$
    SELECT to_timestamp(t / 1e9);
$$;

CREATE FUNCTION ts_nano(t timestamptz) RETURNS BIGINT
LANGUAGE SQL IMMUTABLE AS $$
    SELECT CAST(1000000000 * EXTRACT(EPOCH FROM t) AS BIGINT)
$$;

----------
-- Bookkeeping

-- Small key/value store for things that are not per-block: the schema fingerprint, the chain id
-- we are following, and so on.
CREATE TABLE constants (
    key   TEXT NOT NULL,
    value BYTEA NOT NULL,
    PRIMARY KEY (key)
);

-- One row per block written. The UNIQUE on timestamp is what lets a bucket boundary in seconds
-- be resolved back to a height without a scan.
CREATE TABLE block_log (
    height    BIGINT NOT NULL,
    timestamp BIGINT NOT NULL,
    hash      BYTEA NOT NULL,
    PRIMARY KEY (height),
    UNIQUE (timestamp)
);

-- Defined here rather than with the other helpers because it reads block_log.
CREATE FUNCTION last_height() RETURNS BIGINT
LANGUAGE SQL STABLE AS $$
    SELECT height FROM block_log ORDER BY height DESC LIMIT 1;
$$;

----------
-- Depths

-- Sparse: a row exists only for the heights at which a pool's depth actually changed. To read a
-- depth at time T, take the latest row for that pool at or before T. Asset, rune and synth move
-- together, so a single lookback covers all three.
CREATE TABLE block_pool_depths (
    pool            TEXT NOT NULL,
    asset_e8        BIGINT NOT NULL,
    rune_e8         BIGINT NOT NULL,
    synth_e8        BIGINT NOT NULL,
    units           BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('block_pool_depths');
CREATE INDEX ON block_pool_depths (pool, block_timestamp DESC);

-- RUNE priced in USD, recorded per block from the deepest available anchor pool. Kept as its
-- own table because the choice of anchor pool changes over time with mimir and pool status, and
-- recomputing it retroactively would need the whole mimir history.
CREATE TABLE rune_price (
    price           DOUBLE PRECISION NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('rune_price');

----------
-- Swaps

-- _direction encodes what was swapped for what, so the history endpoint can split volume without
-- re-deriving coin types from the asset strings on every row:
--   0 rune->asset  1 asset->rune  2 rune->synth  3 synth->rune
--   4 rune->trade  5 trade->rune  6 rune->secure 7 secure->rune
CREATE TABLE swap_events (
    tx                 TEXT NOT NULL,
    chain              TEXT NOT NULL,
    from_addr          TEXT NOT NULL,
    to_addr            TEXT NOT NULL,
    from_asset         TEXT NOT NULL,
    from_e8            BIGINT NOT NULL,
    to_asset           TEXT NOT NULL,
    to_e8              BIGINT NOT NULL,
    memo               TEXT NOT NULL,
    pool               TEXT NOT NULL,
    to_e8_min          BIGINT NOT NULL,
    swap_slip_bp       BIGINT NOT NULL,
    liq_fee_e8         BIGINT NOT NULL,
    liq_fee_in_rune_e8 BIGINT NOT NULL,
    _direction         SMALLINT NOT NULL,
    _streaming         BOOLEAN NOT NULL DEFAULT FALSE,
    streaming_count    BIGINT NOT NULL DEFAULT 1,
    streaming_quantity BIGINT NOT NULL DEFAULT 1,
    event_id           BIGINT NOT NULL,
    block_timestamp    BIGINT NOT NULL
);

CALL setup_hypertable('swap_events');
CREATE INDEX ON swap_events (tx);
CREATE INDEX ON swap_events (pool, block_timestamp DESC);
CREATE INDEX ON swap_events (from_addr);

----------
-- Liquidity

-- _asset_in_rune_e8 is the asset side valued in RUNE at the price when the deposit happened,
-- which is the only moment that valuation is knowable. Recomputing it later would silently
-- restate history every time the pool price moved.
CREATE TABLE stake_events (
    pool              TEXT NOT NULL,
    asset_tx          TEXT,
    asset_chain       TEXT,
    asset_addr        TEXT,
    asset_e8          BIGINT NOT NULL,
    stake_units       BIGINT NOT NULL,
    rune_tx           TEXT,
    rune_addr         TEXT,
    rune_e8           BIGINT NOT NULL,
    _asset_in_rune_e8 BIGINT NOT NULL,
    memo              TEXT,
    event_id          BIGINT NOT NULL,
    block_timestamp   BIGINT NOT NULL
);

CALL setup_hypertable('stake_events');
CREATE INDEX ON stake_events (pool, block_timestamp DESC);
CREATE INDEX ON stake_events (rune_addr);
CREATE INDEX ON stake_events (asset_addr);

CREATE TABLE withdraw_events (
    tx                     TEXT NOT NULL,
    chain                  TEXT NOT NULL,
    from_addr              TEXT NOT NULL,
    to_addr                TEXT NOT NULL,
    asset                  TEXT NOT NULL,
    asset_e8               BIGINT NOT NULL,
    emit_asset_e8          BIGINT NOT NULL,
    emit_rune_e8           BIGINT NOT NULL,
    memo                   TEXT NOT NULL,
    pool                   TEXT NOT NULL,
    stake_units            BIGINT NOT NULL,
    basis_points           BIGINT NOT NULL,
    asymmetry              DOUBLE PRECISION NOT NULL,
    imp_loss_protection_e8 BIGINT NOT NULL,
    _emit_asset_in_rune_e8 BIGINT NOT NULL,
    event_id               BIGINT NOT NULL,
    block_timestamp        BIGINT NOT NULL
);

CALL setup_hypertable('withdraw_events');
CREATE INDEX ON withdraw_events (pool, block_timestamp DESC);
CREATE INDEX ON withdraw_events (from_addr);

-- One side of a symmetric deposit arrived and is waiting for its pair.
CREATE TABLE pending_liquidity_events (
    pool            TEXT NOT NULL,
    asset_tx        TEXT,
    asset_chain     TEXT,
    asset_addr      TEXT,
    asset_e8        BIGINT NOT NULL,
    rune_tx         TEXT,
    rune_addr       TEXT,
    rune_e8         BIGINT NOT NULL,
    pending_type    TEXT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('pending_liquidity_events');
CREATE INDEX ON pending_liquidity_events (rune_addr);
CREATE INDEX ON pending_liquidity_events (asset_addr);

----------
-- Fees, rewards, and the rest of the income statement

CREATE TABLE fee_events (
    tx              TEXT NOT NULL,
    asset           TEXT NOT NULL,
    asset_e8        BIGINT NOT NULL,
    pool_deduct     BIGINT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('fee_events');
CREATE INDEX ON fee_events (tx);

CREATE TABLE gas_events (
    asset           TEXT NOT NULL,
    asset_e8        BIGINT NOT NULL,
    rune_e8         BIGINT NOT NULL,
    tx_count        BIGINT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('gas_events');

-- Block rewards. bond_e8 is the node share; the per-pool split lives in the entries table.
CREATE TABLE rewards_events (
    bond_e8         BIGINT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('rewards_events');

-- rune_e8 may be negative: a pool whose share of system income is above target has RUNE taken
-- out of it rather than added.
CREATE TABLE rewards_event_entries (
    pool            TEXT NOT NULL,
    rune_e8         BIGINT NOT NULL,
    saver_e8        BIGINT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('rewards_event_entries');
CREATE INDEX ON rewards_event_entries (pool, block_timestamp DESC);

----------
-- Transfers in and out

CREATE TABLE outbound_events (
    tx              TEXT,
    chain           TEXT NOT NULL,
    from_addr       TEXT NOT NULL,
    to_addr         TEXT NOT NULL,
    asset           TEXT NOT NULL,
    asset_e8        BIGINT NOT NULL,
    memo            TEXT NOT NULL,
    in_tx           TEXT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('outbound_events');
CREATE INDEX ON outbound_events (in_tx);

CREATE TABLE refund_events (
    tx              TEXT NOT NULL,
    chain           TEXT NOT NULL,
    from_addr       TEXT NOT NULL,
    to_addr         TEXT NOT NULL,
    asset           TEXT NOT NULL,
    asset_e8        BIGINT NOT NULL,
    asset_2nd       TEXT,
    asset_2nd_e8    BIGINT NOT NULL,
    memo            TEXT,
    code            BIGINT NOT NULL,
    reason          TEXT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('refund_events');

CREATE TABLE add_events (
    tx              TEXT NOT NULL,
    chain           TEXT NOT NULL,
    from_addr       TEXT NOT NULL,
    to_addr         TEXT NOT NULL,
    asset           TEXT,
    asset_e8        BIGINT NOT NULL,
    memo            TEXT NOT NULL,
    rune_e8         BIGINT NOT NULL,
    pool            TEXT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('add_events');

CREATE TABLE transfer_events (
    from_addr       TEXT NOT NULL,
    to_addr         TEXT NOT NULL,
    asset           TEXT NOT NULL,
    amount_e8       BIGINT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('transfer_events');
CREATE INDEX ON transfer_events (from_addr);
CREATE INDEX ON transfer_events (to_addr);

----------
-- Pool and network state

CREATE TABLE pool_events (
    asset           TEXT NOT NULL,
    status          TEXT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('pool_events');
CREATE INDEX ON pool_events (asset, block_timestamp DESC);

-- Depth adjustments that are not the result of a swap or a deposit: slashes, migrations, and
-- the like. rune_amt/asset_amt are signed.
CREATE TABLE pool_balance_change_events (
    asset            TEXT NOT NULL,
    rune_amt         BIGINT NOT NULL,
    rune_add         BOOLEAN NOT NULL,
    asset_amt        BIGINT NOT NULL,
    asset_add        BOOLEAN NOT NULL,
    reason           TEXT NOT NULL,
    event_id         BIGINT NOT NULL,
    block_timestamp  BIGINT NOT NULL
);

CALL setup_hypertable('pool_balance_change_events');

-- Corrections to a pool's depth after a chain reorg or a mis-observed transaction.
CREATE TABLE errata_events (
    in_tx           TEXT NOT NULL,
    asset           TEXT NOT NULL,
    asset_e8        BIGINT NOT NULL,
    rune_e8         BIGINT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('errata_events');

CREATE TABLE set_mimir_events (
    key             TEXT NOT NULL,
    value           TEXT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('set_mimir_events');
CREATE INDEX ON set_mimir_events (key, block_timestamp DESC);

CREATE TABLE mint_burn_events (
    asset           TEXT NOT NULL,
    asset_e8        BIGINT NOT NULL,
    supply          TEXT NOT NULL,
    reason          TEXT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('mint_burn_events');

----------
-- Nodes and bonding

CREATE TABLE bond_events (
    tx              TEXT NOT NULL,
    chain           TEXT,
    from_addr       TEXT,
    to_addr         TEXT,
    asset           TEXT,
    asset_e8        BIGINT NOT NULL,
    memo            TEXT,
    bond_type       TEXT NOT NULL,
    e8              BIGINT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('bond_events');

CREATE TABLE update_node_account_status_events (
    node_addr       TEXT NOT NULL,
    former          TEXT NOT NULL,
    current         TEXT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('update_node_account_status_events');

CREATE TABLE active_vault_events (
    add_asgard_addr TEXT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('active_vault_events');

CREATE TABLE inactive_vault_events (
    add_asgard_addr TEXT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('inactive_vault_events');

CREATE TABLE slash_events (
    pool            TEXT NOT NULL,
    asset           TEXT NOT NULL,
    asset_e8        BIGINT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('slash_events');

CREATE TABLE switch_events (
    tx              TEXT,
    from_addr       TEXT NOT NULL,
    to_addr         TEXT NOT NULL,
    burn_asset      TEXT NOT NULL,
    mint_asset      TEXT NOT NULL,
    burn_e8         BIGINT NOT NULL,
    mint_e8         BIGINT NOT NULL,
    event_id        BIGINT NOT NULL,
    block_timestamp BIGINT NOT NULL
);

CALL setup_hypertable('switch_events');
