//! Decoding ABCI events into the rows we store.
//!
//! Each struct here mirrors one table in `ddl.sql`. Decoding is deliberately lenient about
//! attributes it does not recognise and strict about the ones it needs: THORNode adds attributes
//! between releases, and refusing an event because it grew a field would lose real data, whereas
//! a missing `pool` on a swap means we genuinely cannot place it.

use midgard_chain::types::{attr, attrs};
use midgard_core::asset::CoinType;
use tendermint::abci::Event;

use crate::coin::{parse_coin, parse_coins, split_rune};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("{event_type}: missing required attribute {attribute:?}")]
    Missing {
        event_type: &'static str,
        attribute: &'static str,
    },

    #[error("{event_type}: attribute {attribute:?} = {value:?} is not a number")]
    NotANumber {
        event_type: &'static str,
        attribute: &'static str,
        value: String,
    },

    #[error("{event_type}: {0}", source)]
    Coin {
        event_type: &'static str,
        #[source]
        source: crate::coin::CoinError,
    },
}

/// Which way a swap went. Stored as `swap_events._direction`.
///
/// Persisted as a number so the history endpoint can split volume with a `WHERE _direction IN
/// (..)` instead of re-deriving coin types from asset strings on every row of a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i16)]
pub enum Direction {
    RuneToAsset = 0,
    AssetToRune = 1,
    RuneToSynth = 2,
    SynthToRune = 3,
    RuneToTrade = 4,
    TradeToRune = 5,
    RuneToSecure = 6,
    SecureToRune = 7,
}

impl Direction {
    /// Classify by the two assets involved.
    ///
    /// Every swap has RUNE on exactly one side — THORChain routes asset-to-asset trades as two
    /// swaps through the pool, and each half arrives as its own event. A pair with RUNE on
    /// neither side (or both) is not something we can classify, so it is left to the caller.
    pub fn classify(from: &str, to: &str) -> Option<Direction> {
        use CoinType::*;
        let (f, t) = (CoinType::of(from), CoinType::of(to));
        Some(match (f, t) {
            (Rune, Synth) => Direction::RuneToSynth,
            (Synth, Rune) => Direction::SynthToRune,
            (Rune, Trade) => Direction::RuneToTrade,
            (Trade, Rune) => Direction::TradeToRune,
            (Rune, Secure) => Direction::RuneToSecure,
            (Secure, Rune) => Direction::SecureToRune,
            // Native and derived assets share the plain asset directions.
            (Rune, Native | Derived) => Direction::RuneToAsset,
            (Native | Derived, Rune) => Direction::AssetToRune,
            _ => return None,
        })
    }

    pub fn as_i16(self) -> i16 {
        self as i16
    }

    /// True for the halves that move value out of RUNE into something else.
    pub fn is_from_rune(self) -> bool {
        matches!(
            self,
            Direction::RuneToAsset
                | Direction::RuneToSynth
                | Direction::RuneToTrade
                | Direction::RuneToSecure
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct Swap {
    pub tx: String,
    pub chain: String,
    pub from_addr: String,
    pub to_addr: String,
    pub from_asset: String,
    pub from_e8: i64,
    pub to_asset: String,
    pub to_e8: i64,
    pub memo: String,
    pub pool: String,
    pub to_e8_min: i64,
    pub swap_slip_bp: i64,
    pub liq_fee_e8: i64,
    pub liq_fee_in_rune_e8: i64,
    pub direction: i16,
    pub streaming: bool,
    pub streaming_count: i64,
    pub streaming_quantity: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Stake {
    pub pool: String,
    pub asset_tx: Option<String>,
    pub asset_chain: Option<String>,
    pub asset_addr: Option<String>,
    pub asset_e8: i64,
    pub stake_units: i64,
    pub rune_tx: Option<String>,
    pub rune_addr: Option<String>,
    pub rune_e8: i64,
    pub asset_in_rune_e8: i64,
    pub memo: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Withdraw {
    pub tx: String,
    pub chain: String,
    pub from_addr: String,
    pub to_addr: String,
    pub asset: String,
    pub asset_e8: i64,
    pub emit_asset_e8: i64,
    pub emit_rune_e8: i64,
    pub memo: String,
    pub pool: String,
    pub stake_units: i64,
    pub basis_points: i64,
    pub asymmetry: f64,
    pub imp_loss_protection_e8: i64,
    pub emit_asset_in_rune_e8: i64,
}

#[derive(Debug, Clone, Default)]
pub struct PoolStatus {
    pub asset: String,
    pub status: String,
}

#[derive(Debug, Clone, Default)]
pub struct Fee {
    pub tx: String,
    pub asset: String,
    pub asset_e8: i64,
    pub pool_deduct: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Gas {
    pub asset: String,
    pub asset_e8: i64,
    pub rune_e8: i64,
    pub tx_count: i64,
}

/// Block rewards. `per_pool` entries may be negative — a pool holding more than its target share
/// of system income has RUNE taken out rather than added.
#[derive(Debug, Clone, Default)]
pub struct Rewards {
    pub bond_e8: i64,
    pub per_pool: Vec<(String, i64)>,
}

#[derive(Debug, Clone, Default)]
pub struct Outbound {
    pub tx: Option<String>,
    pub chain: String,
    pub from_addr: String,
    pub to_addr: String,
    pub asset: String,
    pub asset_e8: i64,
    pub memo: String,
    pub in_tx: String,
}

#[derive(Debug, Clone, Default)]
pub struct Refund {
    pub tx: String,
    pub chain: String,
    pub from_addr: String,
    pub to_addr: String,
    pub asset: String,
    pub asset_e8: i64,
    pub asset_2nd: Option<String>,
    pub asset_2nd_e8: i64,
    pub memo: Option<String>,
    pub code: i64,
    pub reason: String,
}

/// A `donate` event: value given to a pool with nothing asked in return.
#[derive(Debug, Clone, Default)]
pub struct Add {
    pub tx: String,
    pub chain: String,
    pub from_addr: String,
    pub to_addr: String,
    pub asset: Option<String>,
    pub asset_e8: i64,
    pub memo: String,
    pub rune_e8: i64,
    pub pool: String,
}

#[derive(Debug, Clone, Default)]
pub struct Transfer {
    pub from_addr: String,
    pub to_addr: String,
    pub asset: String,
    pub amount_e8: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Bond {
    pub tx: String,
    pub chain: Option<String>,
    pub from_addr: Option<String>,
    pub to_addr: Option<String>,
    pub asset: Option<String>,
    pub asset_e8: i64,
    pub memo: Option<String>,
    pub bond_type: String,
    pub e8: i64,
}

#[derive(Debug, Clone, Default)]
pub struct PendingLiquidity {
    pub pool: String,
    pub asset_tx: Option<String>,
    pub asset_chain: Option<String>,
    pub asset_addr: Option<String>,
    pub asset_e8: i64,
    pub rune_tx: Option<String>,
    pub rune_addr: Option<String>,
    pub rune_e8: i64,
    pub pending_type: String,
}

#[derive(Debug, Clone, Default)]
pub struct Errata {
    pub in_tx: String,
    pub asset: String,
    pub asset_e8: i64,
    pub rune_e8: i64,
}

#[derive(Debug, Clone, Default)]
pub struct SetMimir {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Default)]
pub struct MintBurn {
    pub asset: String,
    pub asset_e8: i64,
    pub supply: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct PoolBalanceChange {
    pub asset: String,
    pub rune_amt: i64,
    pub rune_add: bool,
    pub asset_amt: i64,
    pub asset_add: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct Slash {
    pub pool: String,
    pub asset: String,
    pub asset_e8: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Switch {
    pub tx: Option<String>,
    pub from_addr: String,
    pub to_addr: String,
    pub burn_asset: String,
    pub mint_asset: String,
    pub burn_e8: i64,
    pub mint_e8: i64,
}

#[derive(Debug, Clone, Default)]
pub struct VaultChange {
    pub add_asgard_addr: String,
}

#[derive(Debug, Clone, Default)]
pub struct NodeStatus {
    pub node_addr: String,
    pub former: String,
    pub current: String,
}

/// One decoded event, ready to be written.
#[derive(Debug, Clone)]
pub enum Recorded {
    Swap(Swap),
    Stake(Stake),
    Withdraw(Withdraw),
    Pool(PoolStatus),
    Fee(Fee),
    Gas(Gas),
    Rewards(Rewards),
    Outbound(Outbound),
    Refund(Refund),
    Add(Add),
    Transfer(Transfer),
    Bond(Bond),
    PendingLiquidity(PendingLiquidity),
    Errata(Errata),
    SetMimir(SetMimir),
    MintBurn(MintBurn),
    PoolBalanceChange(PoolBalanceChange),
    Slash(Slash),
    Switch(Switch),
    ActiveVault(VaultChange),
    InactiveVault(VaultChange),
    NodeStatus(NodeStatus),
}

/// Event types we knowingly ignore.
///
/// Listing them explicitly is what lets [`decode`] distinguish "not interesting" from "we have
/// never seen this", so a genuinely new THORChain event type shows up in the logs instead of
/// vanishing into a catch-all.
pub const IGNORED: &[&str] = &[
    "tx",
    "message",
    "coin_spent",
    "coin_received",
    "coinbase",
    "burn",
    "mint",
    "tss_keygen",
    "tss_keysign",
    "create_client",
    "update_client",
    "connection_open_init",
    "store_code",
    "pin_code",
    "security",
    "execute",
    "instantiate",
    "reply",
    "wasm",
    "oracle_price",
    "approve_upgrade",
    "schedule_start",
    "schedule_add",
    "schedule_end",
    "set_ip_address",
    "set_node_keys",
    "set_version",
    "validator_request_leave",
    "new_node",
    "asgard_fund_yggdrasil",
    "slash_points",
    "set_node_mimir",
    "version",
    "scheduled_outbound",
];

/// The outcome of looking at one event.
#[derive(Debug)]
pub enum Decoded {
    /// Store it. The overwhelmingly common case, kept as a single boxed value rather than a
    /// `Vec` so the hot path does not allocate a heap vector per event.
    Event(Box<Recorded>),
    /// Store all of them. One ABCI event can legitimately describe several rows: a bank transfer
    /// moving more than one denomination at once, for instance. They share an event id.
    Events(Vec<Recorded>),
    /// A known type we do not store.
    Ignored,
    /// A type we have never seen. Worth a log line.
    Unknown,
}

/// Decode one ABCI event.
pub fn decode(event: &Event) -> Result<Decoded, DecodeError> {
    let kind = event.kind.as_str();
    let rec = match kind {
        "swap" => Recorded::Swap(swap(event)?),
        "add_liquidity" | "add" => Recorded::Stake(stake(event)?),
        "withdraw" => Recorded::Withdraw(withdraw(event)?),
        "pool" => Recorded::Pool(pool_status(event)?),
        "fee" => Recorded::Fee(fee(event)?),
        "gas" => Recorded::Gas(gas(event)?),
        "rewards" => Recorded::Rewards(rewards(event)?),
        "outbound" => Recorded::Outbound(outbound(event)?),
        "refund" => Recorded::Refund(refund(event)?),
        "donate" => Recorded::Add(donate(event)?),
        // The only type that can yield more than one row from one event.
        "transfer" => {
            return Ok(Decoded::Events(
                transfer(event)?
                    .into_iter()
                    .map(Recorded::Transfer)
                    .collect(),
            ))
        }
        "bond" => Recorded::Bond(bond(event)?),
        "pending_liquidity" => Recorded::PendingLiquidity(pending_liquidity(event)?),
        "errata" => Recorded::Errata(errata(event)?),
        "set_mimir" => Recorded::SetMimir(set_mimir(event)?),
        "mint_burn" => Recorded::MintBurn(mint_burn(event)?),
        "pool_balance_change" => Recorded::PoolBalanceChange(pool_balance_change(event)?),
        "slash" => Recorded::Slash(slash(event)?),
        "switch" => Recorded::Switch(switch(event)?),
        "ActiveVault" => Recorded::ActiveVault(vault(event)?),
        "InactiveVault" => Recorded::InactiveVault(vault(event)?),
        "UpdateNodeAccountStatus" => Recorded::NodeStatus(node_status(event)?),
        other if IGNORED.contains(&other) => return Ok(Decoded::Ignored),
        _ => return Ok(Decoded::Unknown),
    };
    Ok(Decoded::Event(Box::new(rec)))
}

// -- attribute helpers -------------------------------------------------------

fn required<'e>(
    event: &'e Event,
    kind: &'static str,
    key: &'static str,
) -> Result<&'e str, DecodeError> {
    attr(event, key).ok_or(DecodeError::Missing {
        event_type: kind,
        attribute: key,
    })
}

fn optional(event: &Event, key: &str) -> Option<String> {
    attr(event, key)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn text(event: &Event, key: &str) -> String {
    attr(event, key).unwrap_or_default().to_string()
}

/// An integer attribute, defaulting to zero when absent.
///
/// Absent and zero are the same thing for every numeric attribute THORChain emits, and treating
/// them differently would mean a new optional field breaks decoding of every prior event.
fn int(event: &Event, kind: &'static str, key: &'static str) -> Result<i64, DecodeError> {
    match attr(event, key) {
        None | Some("") => Ok(0),
        Some(v) => v.parse().map_err(|_| DecodeError::NotANumber {
            event_type: kind,
            attribute: key,
            value: v.to_string(),
        }),
    }
}

fn float(event: &Event, kind: &'static str, key: &'static str) -> Result<f64, DecodeError> {
    match attr(event, key) {
        None | Some("") => Ok(0.0),
        Some(v) => v.parse().map_err(|_| DecodeError::NotANumber {
            event_type: kind,
            attribute: key,
            value: v.to_string(),
        }),
    }
}

fn coin_of(
    event: &Event,
    kind: &'static str,
    key: &'static str,
) -> Result<crate::coin::Coin, DecodeError> {
    let raw = required(event, kind, key)?;
    parse_coin(raw).map_err(|source| DecodeError::Coin {
        event_type: kind,
        source,
    })
}

// -- per-event decoders ------------------------------------------------------

fn swap(e: &Event) -> Result<Swap, DecodeError> {
    const K: &str = "swap";
    let from = coin_of(e, K, "coin")?;
    let to = coin_of(e, K, "emit_asset")?;

    // A swap whose assets we cannot classify is still recorded; the direction column just does
    // not participate in the volume split. Dropping it would lose the action from the feed.
    let direction = Direction::classify(&from.asset, &to.asset)
        .map(Direction::as_i16)
        .unwrap_or(Direction::RuneToAsset.as_i16());

    let quantity = int(e, K, "streaming_swap_quantity")?.max(1);
    let count = int(e, K, "streaming_swap_count")?.max(1);

    Ok(Swap {
        tx: text(e, "id"),
        chain: text(e, "chain"),
        from_addr: text(e, "from"),
        to_addr: text(e, "to"),
        from_asset: from.asset,
        from_e8: from.amount_e8,
        to_asset: to.asset,
        to_e8: to.amount_e8,
        memo: text(e, "memo"),
        pool: required(e, K, "pool")?.to_string(),
        to_e8_min: int(e, K, "swap_target")?,
        swap_slip_bp: int(e, K, "swap_slip")?,
        liq_fee_e8: int(e, K, "liquidity_fee")?,
        liq_fee_in_rune_e8: int(e, K, "liquidity_fee_in_rune")?,
        direction,
        // More than one planned sub-swap is what makes it a streaming swap.
        streaming: quantity > 1,
        streaming_count: count,
        streaming_quantity: quantity,
    })
}

fn stake(e: &Event) -> Result<Stake, DecodeError> {
    const K: &str = "add_liquidity";
    Ok(Stake {
        pool: required(e, K, "pool")?.to_string(),
        asset_tx: optional(e, "asset_tx_id"),
        asset_chain: optional(e, "asset_chain"),
        asset_addr: optional(e, "asset_address"),
        asset_e8: int(e, K, "asset_amount")?,
        // THORNode renamed this from stake_units; accept both so historical blocks decode.
        stake_units: match attr(e, "liquidity_provider_units") {
            Some(_) => int(e, K, "liquidity_provider_units")?,
            None => int(e, K, "stake_units")?,
        },
        rune_tx: optional(e, "rune_tx_id"),
        rune_addr: optional(e, "rune_address"),
        rune_e8: int(e, K, "rune_amount")?,
        // Filled in by the recorder, which knows the pool price at this height.
        asset_in_rune_e8: 0,
        memo: optional(e, "memo"),
    })
}

fn withdraw(e: &Event) -> Result<Withdraw, DecodeError> {
    const K: &str = "withdraw";
    let coin = coin_of(e, K, "coin")?;
    Ok(Withdraw {
        tx: text(e, "id"),
        chain: text(e, "chain"),
        from_addr: text(e, "from"),
        to_addr: text(e, "to"),
        asset: coin.asset,
        asset_e8: coin.amount_e8,
        emit_asset_e8: int(e, K, "emit_asset")?,
        emit_rune_e8: int(e, K, "emit_rune")?,
        memo: text(e, "memo"),
        pool: required(e, K, "pool")?.to_string(),
        stake_units: int(e, K, "liquidity_provider_units")?,
        basis_points: int(e, K, "basis_points")?,
        asymmetry: float(e, K, "asymmetry")?,
        imp_loss_protection_e8: int(e, K, "imp_loss_protection")?,
        emit_asset_in_rune_e8: 0,
    })
}

fn pool_status(e: &Event) -> Result<PoolStatus, DecodeError> {
    const K: &str = "pool";
    Ok(PoolStatus {
        asset: required(e, K, "pool")?.to_string(),
        // Normalised because THORNode has used both "Available" and "available".
        status: required(e, K, "pool_status")?.to_ascii_lowercase(),
    })
}

fn fee(e: &Event) -> Result<Fee, DecodeError> {
    const K: &str = "fee";
    let coin = coin_of(e, K, "coins")?;
    Ok(Fee {
        tx: text(e, "tx_id"),
        asset: coin.asset,
        asset_e8: coin.amount_e8,
        pool_deduct: int(e, K, "pool_deduct")?,
    })
}

fn gas(e: &Event) -> Result<Gas, DecodeError> {
    const K: &str = "gas";
    let coin = coin_of(e, K, "asset").or_else(|_| -> Result<_, DecodeError> {
        Ok(crate::coin::Coin {
            asset: text(e, "asset"),
            amount_e8: int(e, K, "asset_amt")?,
        })
    })?;
    Ok(Gas {
        asset: coin.asset,
        asset_e8: coin.amount_e8,
        rune_e8: int(e, K, "rune_amt")?,
        tx_count: int(e, K, "transaction_count")?,
    })
}

fn rewards(e: &Event) -> Result<Rewards, DecodeError> {
    const K: &str = "rewards";
    let mut per_pool = Vec::new();
    for (key, value) in attrs(e) {
        // Every attribute other than the two known ones is "<pool>" = "<rune_e8>". There is no
        // list of pools in the event, so the pool names have to be read off the keys.
        if key == "bond_reward" || key == "mode" {
            continue;
        }
        match value.parse::<i64>() {
            Ok(amount) => per_pool.push((key.to_string(), amount)),
            Err(_) => tracing::debug!(key, value, "unparseable rewards entry, skipped"),
        }
    }
    Ok(Rewards {
        bond_e8: int(e, K, "bond_reward")?,
        per_pool,
    })
}

fn outbound(e: &Event) -> Result<Outbound, DecodeError> {
    const K: &str = "outbound";
    let coin = coin_of(e, K, "coin")?;
    Ok(Outbound {
        tx: optional(e, "id"),
        chain: text(e, "chain"),
        from_addr: text(e, "from"),
        to_addr: text(e, "to"),
        asset: coin.asset,
        asset_e8: coin.amount_e8,
        memo: text(e, "memo"),
        in_tx: required(e, K, "in_tx_id")?.to_string(),
    })
}

fn refund(e: &Event) -> Result<Refund, DecodeError> {
    const K: &str = "refund";
    let coins = parse_coins(&text(e, "coin")).map_err(|source| DecodeError::Coin {
        event_type: K,
        source,
    })?;
    let first = coins.first().cloned().unwrap_or_default();
    let second = coins.get(1);

    Ok(Refund {
        tx: text(e, "id"),
        chain: text(e, "chain"),
        from_addr: text(e, "from"),
        to_addr: text(e, "to"),
        asset: first.asset,
        asset_e8: first.amount_e8,
        asset_2nd: second.map(|c| c.asset.clone()),
        asset_2nd_e8: second.map(|c| c.amount_e8).unwrap_or(0),
        memo: optional(e, "memo"),
        code: int(e, K, "code")?,
        reason: text(e, "reason"),
    })
}

fn donate(e: &Event) -> Result<Add, DecodeError> {
    const K: &str = "donate";
    let coins = parse_coins(&text(e, "coin")).map_err(|source| DecodeError::Coin {
        event_type: K,
        source,
    })?;
    let (rune_e8, asset) = split_rune(&coins);

    Ok(Add {
        tx: text(e, "id"),
        chain: text(e, "chain"),
        from_addr: text(e, "from"),
        to_addr: text(e, "to"),
        asset: asset.map(|c| c.asset.clone()),
        asset_e8: asset.map(|c| c.amount_e8).unwrap_or(0),
        memo: text(e, "memo"),
        rune_e8,
        pool: required(e, K, "pool")?.to_string(),
    })
}

fn transfer(e: &Event) -> Result<Vec<Transfer>, DecodeError> {
    const K: &str = "transfer";
    // These come from the Cosmos bank module, not from THORChain, so the amount is in the SDK's
    // "<amount><denom>" spelling rather than THORChain's "<amount> <ASSET>". Parsing them with
    // the wrong one silently drops every transfer on the chain.
    //
    // It can also be a list — one transfer moving several denominations — which becomes one row
    // per coin.
    let raw = required(e, K, "amount")?;
    let coins = crate::coin::parse_cosmos_coins(raw).map_err(|source| DecodeError::Coin {
        event_type: K,
        source,
    })?;

    let from_addr = text(e, "sender");
    let to_addr = text(e, "recipient");

    Ok(coins
        .into_iter()
        .map(|coin| Transfer {
            from_addr: from_addr.clone(),
            to_addr: to_addr.clone(),
            asset: coin.asset,
            amount_e8: coin.amount_e8,
        })
        .collect())
}

fn bond(e: &Event) -> Result<Bond, DecodeError> {
    const K: &str = "bond";
    let coin = coin_of(e, K, "coin").ok();
    Ok(Bond {
        tx: text(e, "id"),
        chain: optional(e, "chain"),
        from_addr: optional(e, "from"),
        to_addr: optional(e, "to"),
        asset: coin.as_ref().map(|c| c.asset.clone()),
        asset_e8: coin.as_ref().map(|c| c.amount_e8).unwrap_or(0),
        memo: optional(e, "memo"),
        bond_type: text(e, "bond_type"),
        e8: int(e, K, "amount")?,
    })
}

fn pending_liquidity(e: &Event) -> Result<PendingLiquidity, DecodeError> {
    const K: &str = "pending_liquidity";
    Ok(PendingLiquidity {
        pool: required(e, K, "pool")?.to_string(),
        asset_tx: optional(e, "asset_tx_id"),
        asset_chain: optional(e, "asset_chain"),
        asset_addr: optional(e, "asset_address"),
        asset_e8: int(e, K, "asset_amount")?,
        rune_tx: optional(e, "rune_tx_id"),
        rune_addr: optional(e, "rune_address"),
        rune_e8: int(e, K, "rune_amount")?,
        pending_type: text(e, "type"),
    })
}

fn errata(e: &Event) -> Result<Errata, DecodeError> {
    const K: &str = "errata";
    // The add flags say whether the correction is positive or negative.
    let rune = int(e, K, "rune_amt")?;
    let asset = int(e, K, "asset_amt")?;
    let signed = |v: i64, add: &str| if attr(e, add) == Some("false") { -v } else { v };

    Ok(Errata {
        in_tx: text(e, "in_tx_id"),
        asset: text(e, "asset"),
        asset_e8: signed(asset, "asset_add"),
        rune_e8: signed(rune, "rune_add"),
    })
}

fn set_mimir(e: &Event) -> Result<SetMimir, DecodeError> {
    const K: &str = "set_mimir";
    Ok(SetMimir {
        key: required(e, K, "key")?.to_string(),
        value: text(e, "value"),
    })
}

fn mint_burn(e: &Event) -> Result<MintBurn, DecodeError> {
    const K: &str = "mint_burn";
    Ok(MintBurn {
        asset: required(e, K, "denom")?.to_string(),
        asset_e8: int(e, K, "amount")?,
        supply: text(e, "supply"),
        reason: text(e, "reason"),
    })
}

fn pool_balance_change(e: &Event) -> Result<PoolBalanceChange, DecodeError> {
    const K: &str = "pool_balance_change";
    Ok(PoolBalanceChange {
        asset: required(e, K, "asset")?.to_string(),
        rune_amt: int(e, K, "rune_amt")?,
        rune_add: attr(e, "rune_add") != Some("false"),
        asset_amt: int(e, K, "asset_amt")?,
        asset_add: attr(e, "asset_add") != Some("false"),
        reason: text(e, "reason"),
    })
}

fn slash(e: &Event) -> Result<Slash, DecodeError> {
    const K: &str = "slash";
    let pool = required(e, K, "pool")?.to_string();
    // The slashed amounts are extra attributes keyed by asset name, same trick as rewards.
    let (asset, asset_e8) = attrs(e)
        .filter(|(k, _)| *k != "pool" && *k != "mode")
        .find_map(|(k, v)| v.parse::<i64>().ok().map(|n| (k.to_string(), n)))
        .unwrap_or_default();

    Ok(Slash {
        pool,
        asset,
        asset_e8,
    })
}

fn switch(e: &Event) -> Result<Switch, DecodeError> {
    const K: &str = "switch";
    let burn = coin_of(e, K, "burn").ok();
    let mint = coin_of(e, K, "mint").ok();
    Ok(Switch {
        tx: optional(e, "txid"),
        from_addr: text(e, "from"),
        to_addr: text(e, "to"),
        burn_asset: burn.as_ref().map(|c| c.asset.clone()).unwrap_or_default(),
        burn_e8: burn.as_ref().map(|c| c.amount_e8).unwrap_or(0),
        mint_asset: mint.as_ref().map(|c| c.asset.clone()).unwrap_or_default(),
        mint_e8: mint.as_ref().map(|c| c.amount_e8).unwrap_or(0),
    })
}

/// Vault rotation. The attribute holding the address has been spelled two ways across THORNode
/// versions, so both are accepted.
fn vault(e: &Event) -> Result<VaultChange, DecodeError> {
    let addr = attr(e, "add new asgard vault")
        .or_else(|| attr(e, "set_asgard_vault"))
        .unwrap_or_default();
    Ok(VaultChange {
        add_asgard_addr: addr.to_string(),
    })
}

fn node_status(e: &Event) -> Result<NodeStatus, DecodeError> {
    Ok(NodeStatus {
        node_addr: text(e, "Address"),
        former: text(e, "Former:"),
        current: text(e, "Current:"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use midgard_chain::types::make_event;

    #[test]
    fn decodes_a_real_swap() {
        // Verbatim from THORChain mainnet, height 27260503.
        let e = make_event(
            "swap",
            &[
                ("pool", "THOR.TCY"),
                ("swap_target", "0"),
                ("swap_slip", "0"),
                ("liquidity_fee", "0"),
                ("liquidity_fee_in_rune", "0"),
                ("emit_asset", "79693 THOR.RUNE"),
                ("streaming_swap_quantity", "1"),
                ("streaming_swap_count", "1"),
                ("pool_slip", "0"),
                (
                    "id",
                    "BFD5AA829B804F6BF3378D57ACEB64B9EDBD58D64D9290721CF359F26CD438E3",
                ),
                ("chain", "THOR"),
                (
                    "from",
                    "thor1n5a08r0zvmqca39ka2tgwlkjy9ugalutk7fjpzptfppqcccnat2ska5t4g",
                ),
                ("to", "thor1g98cy3n9mmjrpn0sxmn63lztelera37n8n67c0"),
                ("coin", "250000 THOR.TCY"),
                ("memo", "=:ETH-USDC:thor1abc:0/1/1"),
                ("mode", "EndBlock"),
            ],
        );

        let Decoded::Event(rec) = decode(&e).unwrap() else {
            panic!("swap should decode")
        };
        let Recorded::Swap(s) = *rec else {
            panic!("wrong variant")
        };

        assert_eq!(s.pool, "THOR.TCY");
        assert_eq!(s.from_asset, "THOR.TCY");
        assert_eq!(s.from_e8, 250_000);
        assert_eq!(s.to_asset, "THOR.RUNE");
        assert_eq!(s.to_e8, 79_693);
        assert_eq!(s.chain, "THOR");
        assert!(!s.streaming);
        assert_eq!(s.streaming_quantity, 1);
        // THOR.TCY is a native token, not a derived asset, so this is asset -> rune.
        assert_eq!(s.direction, Direction::AssetToRune.as_i16());
    }

    #[test]
    fn swap_directions_cover_every_asset_flavour() {
        use Direction::*;
        let cases = [
            ("THOR.RUNE", "BTC.BTC", RuneToAsset),
            ("BTC.BTC", "THOR.RUNE", AssetToRune),
            ("THOR.RUNE", "BTC/BTC", RuneToSynth),
            ("BTC/BTC", "THOR.RUNE", SynthToRune),
            ("THOR.RUNE", "BTC~BTC", RuneToTrade),
            ("BTC~BTC", "THOR.RUNE", TradeToRune),
            ("THOR.RUNE", "BTC-BTC", RuneToSecure),
            ("BTC-BTC", "THOR.RUNE", SecureToRune),
            ("THOR.RUNE", "THOR.BTC", RuneToAsset),
        ];
        for (from, to, want) in cases {
            assert_eq!(Direction::classify(from, to), Some(want), "{from} -> {to}");
        }
    }

    #[test]
    fn a_swap_with_rune_on_neither_side_is_unclassifiable() {
        assert_eq!(Direction::classify("BTC.BTC", "ETH.ETH"), None);
    }

    #[test]
    fn from_rune_directions_are_the_odd_ones_out() {
        assert!(Direction::RuneToAsset.is_from_rune());
        assert!(Direction::RuneToSecure.is_from_rune());
        assert!(!Direction::AssetToRune.is_from_rune());
        assert!(!Direction::SynthToRune.is_from_rune());
    }

    #[test]
    fn streaming_is_inferred_from_the_planned_quantity() {
        let mk = |q: &str, c: &str| {
            make_event(
                "swap",
                &[
                    ("pool", "BTC.BTC"),
                    ("coin", "1 THOR.RUNE"),
                    ("emit_asset", "1 BTC.BTC"),
                    ("streaming_swap_quantity", q),
                    ("streaming_swap_count", c),
                ],
            )
        };
        let get = |e| match decode(&e).unwrap() {
            Decoded::Event(r) => match *r {
                Recorded::Swap(s) => s,
                _ => panic!(),
            },
            _ => panic!(),
        };

        assert!(!get(mk("1", "1")).streaming);
        let s = get(mk("10", "3"));
        assert!(s.streaming);
        assert_eq!(s.streaming_quantity, 10);
        assert_eq!(s.streaming_count, 3);
    }

    #[test]
    fn absent_streaming_attributes_default_to_one_not_zero() {
        // A zero quantity would make the history endpoint divide by it.
        let e = make_event(
            "swap",
            &[
                ("pool", "BTC.BTC"),
                ("coin", "1 THOR.RUNE"),
                ("emit_asset", "1 BTC.BTC"),
            ],
        );
        let Decoded::Event(r) = decode(&e).unwrap() else {
            panic!()
        };
        let Recorded::Swap(s) = *r else { panic!() };
        assert_eq!(s.streaming_quantity, 1);
        assert_eq!(s.streaming_count, 1);
    }

    #[test]
    fn a_swap_without_a_pool_is_refused() {
        let e = make_event(
            "swap",
            &[("coin", "1 THOR.RUNE"), ("emit_asset", "1 BTC.BTC")],
        );
        let err = decode(&e).unwrap_err();
        assert!(err.to_string().contains("pool"), "{err}");
    }

    #[test]
    fn rewards_reads_pool_names_off_the_attribute_keys() {
        let e = make_event(
            "rewards",
            &[
                ("bond_reward", "1000"),
                ("BTC.BTC", "500"),
                ("ETH.ETH", "-250"),
                ("mode", "BeginBlock"),
            ],
        );
        let Decoded::Event(r) = decode(&e).unwrap() else {
            panic!()
        };
        let Recorded::Rewards(rw) = *r else { panic!() };

        assert_eq!(rw.bond_e8, 1_000);
        assert_eq!(rw.per_pool.len(), 2);
        assert_eq!(rw.per_pool[0], ("BTC.BTC".to_string(), 500));
        // Negative pool rewards are real: a pool above its target share gets RUNE taken out.
        assert_eq!(rw.per_pool[1], ("ETH.ETH".to_string(), -250));
    }

    #[test]
    fn add_liquidity_accepts_both_unit_attribute_spellings() {
        let old = make_event(
            "add_liquidity",
            &[
                ("pool", "BTC.BTC"),
                ("stake_units", "42"),
                ("rune_amount", "1"),
            ],
        );
        let new = make_event(
            "add_liquidity",
            &[
                ("pool", "BTC.BTC"),
                ("liquidity_provider_units", "42"),
                ("rune_amount", "1"),
            ],
        );
        for e in [old, new] {
            let Decoded::Event(r) = decode(&e).unwrap() else {
                panic!()
            };
            let Recorded::Stake(s) = *r else { panic!() };
            assert_eq!(s.stake_units, 42);
        }
    }

    #[test]
    fn pool_status_is_normalised_to_lowercase() {
        for spelling in ["Available", "available", "AVAILABLE"] {
            let e = make_event("pool", &[("pool", "BTC.BTC"), ("pool_status", spelling)]);
            let Decoded::Event(r) = decode(&e).unwrap() else {
                panic!()
            };
            let Recorded::Pool(p) = *r else { panic!() };
            assert_eq!(p.status, "available", "{spelling}");
        }
    }

    #[test]
    fn errata_sign_follows_the_add_flags() {
        let e = make_event(
            "errata",
            &[
                ("in_tx_id", "ABC"),
                ("asset", "BTC.BTC"),
                ("asset_amt", "100"),
                ("asset_add", "false"),
                ("rune_amt", "50"),
                ("rune_add", "true"),
            ],
        );
        let Decoded::Event(r) = decode(&e).unwrap() else {
            panic!()
        };
        let Recorded::Errata(er) = *r else { panic!() };
        assert_eq!(er.asset_e8, -100);
        assert_eq!(er.rune_e8, 50);
    }

    #[test]
    fn donate_splits_rune_from_the_asset_side() {
        let e = make_event(
            "donate",
            &[
                ("pool", "BTC.BTC"),
                ("coin", "100 BTC.BTC, 5000 THOR.RUNE"),
                ("id", "TX"),
            ],
        );
        let Decoded::Event(r) = decode(&e).unwrap() else {
            panic!()
        };
        let Recorded::Add(a) = *r else { panic!() };
        assert_eq!(a.rune_e8, 5_000);
        assert_eq!(a.asset.as_deref(), Some("BTC.BTC"));
        assert_eq!(a.asset_e8, 100);
    }

    #[test]
    fn known_uninteresting_types_are_ignored_not_unknown() {
        for kind in ["coin_spent", "message", "wasm", "schedule_start"] {
            assert!(
                matches!(decode(&make_event(kind, &[])), Ok(Decoded::Ignored)),
                "{kind} should be ignored"
            );
        }
    }

    #[test]
    fn genuinely_new_types_report_as_unknown() {
        // The point of the IGNORED list: a new THORChain event should be visible, not silent.
        assert!(matches!(
            decode(&make_event("some_brand_new_event", &[])),
            Ok(Decoded::Unknown)
        ));
    }

    #[test]
    fn missing_numeric_attributes_default_to_zero() {
        let e = make_event("fee", &[("coins", "10 BTC.BTC"), ("tx_id", "ABC")]);
        let Decoded::Event(r) = decode(&e).unwrap() else {
            panic!()
        };
        let Recorded::Fee(f) = *r else { panic!() };
        assert_eq!(f.pool_deduct, 0);
        assert_eq!(f.asset_e8, 10);
    }

    #[test]
    fn a_non_numeric_attribute_is_an_error_not_a_zero() {
        let e = make_event("fee", &[("coins", "10 BTC.BTC"), ("pool_deduct", "lots")]);
        let err = decode(&e).unwrap_err();
        assert!(err.to_string().contains("pool_deduct"), "{err}");
    }
}
