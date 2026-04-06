//! Response bodies.
//!
//! Field names and types follow Midgard's OpenAPI spec exactly, including the part that looks
//! wrong at first glance: **every numeric field is a JSON string**. Pool depths pass 2^53 and
//! would lose precision in any JavaScript client that received them as JSON numbers, so the wire
//! format has always been strings. Changing that would break every consumer.

use serde::Serialize;

/// Height paired with the block time at that height.
#[derive(Debug, Serialize, Default)]
pub struct HeightTs {
    pub height: i64,
    pub timestamp: i64,
}

#[derive(Debug, Serialize)]
pub struct Health {
    pub database: bool,
    #[serde(rename = "inSync")]
    pub in_sync: bool,
    #[serde(rename = "scannerHeight")]
    pub scanner_height: String,
    #[serde(rename = "lastCommitted")]
    pub last_committed: HeightTs,
    #[serde(rename = "lastFetched")]
    pub last_fetched: HeightTs,
    #[serde(rename = "lastThorNode")]
    pub last_thornode: HeightTs,
    #[serde(rename = "lastAggregated")]
    pub last_aggregated: HeightTs,
}

#[derive(Debug, Serialize, Default)]
pub struct PoolDetail {
    pub asset: String,
    pub status: String,
    #[serde(rename = "assetDepth")]
    pub asset_depth: String,
    #[serde(rename = "runeDepth")]
    pub rune_depth: String,
    #[serde(rename = "assetPrice")]
    pub asset_price: String,
    #[serde(rename = "assetPriceUSD")]
    pub asset_price_usd: String,
    #[serde(rename = "liquidityUnits")]
    pub liquidity_units: String,
    #[serde(rename = "synthUnits")]
    pub synth_units: String,
    #[serde(rename = "synthSupply")]
    pub synth_supply: String,
    pub units: String,
    #[serde(rename = "nativeDecimal")]
    pub native_decimal: String,
    #[serde(rename = "saversDepth")]
    pub savers_depth: String,
    #[serde(rename = "saversUnits")]
    pub savers_units: String,
    #[serde(rename = "volume24h")]
    pub volume_24h: String,
    #[serde(rename = "annualPercentageRate")]
    pub annual_percentage_rate: String,
    #[serde(rename = "poolAPY")]
    pub pool_apy: String,
    #[serde(rename = "earnings")]
    pub earnings: String,
    #[serde(rename = "earningsAnnualAsPercentOfDepth")]
    pub earnings_annual_as_percent_of_depth: String,
    #[serde(rename = "liquidityInUSD")]
    pub liquidity_in_usd: String,
    #[serde(rename = "lpLuvi")]
    pub lp_luvi: String,
    #[serde(rename = "saversAPR")]
    pub savers_apr: String,
    #[serde(rename = "totalCollateral")]
    pub total_collateral: String,
    #[serde(rename = "totalDebtTor")]
    pub total_debt_tor: String,
}

/// `{ "intervals": [...], "meta": {...} }`, the shape every history endpoint returns.
#[derive(Debug, Serialize)]
pub struct History<T> {
    pub intervals: Vec<T>,
    pub meta: T,
}

#[derive(Debug, Serialize, Default, Clone)]
pub struct DepthHistoryItem {
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "endTime")]
    pub end_time: String,
    #[serde(rename = "assetDepth")]
    pub asset_depth: String,
    #[serde(rename = "runeDepth")]
    pub rune_depth: String,
    #[serde(rename = "assetPrice")]
    pub asset_price: String,
    #[serde(rename = "assetPriceUSD")]
    pub asset_price_usd: String,
    #[serde(rename = "liquidityUnits")]
    pub liquidity_units: String,
    #[serde(rename = "synthUnits")]
    pub synth_units: String,
    #[serde(rename = "synthSupply")]
    pub synth_supply: String,
    pub units: String,
    #[serde(rename = "membersCount")]
    pub members_count: String,
    pub luvi: String,
}

/// `meta` for depth history carries start/end snapshots rather than another interval.
#[derive(Debug, Serialize, Default)]
pub struct DepthHistoryMeta {
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "endTime")]
    pub end_time: String,
    #[serde(rename = "startAssetDepth")]
    pub start_asset_depth: String,
    #[serde(rename = "startRuneDepth")]
    pub start_rune_depth: String,
    #[serde(rename = "startLPUnits")]
    pub start_lp_units: String,
    #[serde(rename = "startSynthUnits")]
    pub start_synth_units: String,
    #[serde(rename = "startMemberCount")]
    pub start_member_count: String,
    #[serde(rename = "endAssetDepth")]
    pub end_asset_depth: String,
    #[serde(rename = "endRuneDepth")]
    pub end_rune_depth: String,
    #[serde(rename = "endLPUnits")]
    pub end_lp_units: String,
    #[serde(rename = "endSynthUnits")]
    pub end_synth_units: String,
    #[serde(rename = "endMemberCount")]
    pub end_member_count: String,
    #[serde(rename = "luviIncrease")]
    pub luvi_increase: String,
    #[serde(rename = "priceShiftLoss")]
    pub price_shift_loss: String,
}

#[derive(Debug, Serialize)]
pub struct DepthHistory {
    pub intervals: Vec<DepthHistoryItem>,
    pub meta: DepthHistoryMeta,
}

#[derive(Debug, Serialize, Default, Clone)]
pub struct SwapHistoryItem {
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "endTime")]
    pub end_time: String,

    #[serde(rename = "toAssetCount")]
    pub to_asset_count: String,
    #[serde(rename = "toRuneCount")]
    pub to_rune_count: String,
    #[serde(rename = "synthMintCount")]
    pub synth_mint_count: String,
    #[serde(rename = "synthRedeemCount")]
    pub synth_redeem_count: String,
    #[serde(rename = "totalCount")]
    pub total_count: String,

    #[serde(rename = "toAssetVolume")]
    pub to_asset_volume: String,
    #[serde(rename = "toRuneVolume")]
    pub to_rune_volume: String,
    #[serde(rename = "synthMintVolume")]
    pub synth_mint_volume: String,
    #[serde(rename = "synthRedeemVolume")]
    pub synth_redeem_volume: String,
    #[serde(rename = "totalVolume")]
    pub total_volume: String,

    #[serde(rename = "toAssetFees")]
    pub to_asset_fees: String,
    #[serde(rename = "toRuneFees")]
    pub to_rune_fees: String,
    #[serde(rename = "synthMintFees")]
    pub synth_mint_fees: String,
    #[serde(rename = "synthRedeemFees")]
    pub synth_redeem_fees: String,
    #[serde(rename = "totalFees")]
    pub total_fees: String,

    #[serde(rename = "toAssetAverageSlip")]
    pub to_asset_average_slip: String,
    #[serde(rename = "toRuneAverageSlip")]
    pub to_rune_average_slip: String,
    #[serde(rename = "synthMintAverageSlip")]
    pub synth_mint_average_slip: String,
    #[serde(rename = "synthRedeemAverageSlip")]
    pub synth_redeem_average_slip: String,
    #[serde(rename = "averageSlip")]
    pub average_slip: String,

    #[serde(rename = "runePriceUSD")]
    pub rune_price_usd: String,
}

#[derive(Debug, Serialize, Default, Clone)]
pub struct EarningsHistoryItemPool {
    pub pool: String,
    #[serde(rename = "assetLiquidityFees")]
    pub asset_liquidity_fees: String,
    #[serde(rename = "runeLiquidityFees")]
    pub rune_liquidity_fees: String,
    #[serde(rename = "totalLiquidityFeesRune")]
    pub total_liquidity_fees_rune: String,
    pub rewards: String,
    pub earnings: String,
    #[serde(rename = "saverEarning")]
    pub saver_earning: String,
}

#[derive(Debug, Serialize, Default, Clone)]
pub struct EarningsHistoryItem {
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "endTime")]
    pub end_time: String,
    #[serde(rename = "liquidityFees")]
    pub liquidity_fees: String,
    #[serde(rename = "blockRewards")]
    pub block_rewards: String,
    pub earnings: String,
    #[serde(rename = "bondingEarnings")]
    pub bonding_earnings: String,
    #[serde(rename = "liquidityEarnings")]
    pub liquidity_earnings: String,
    #[serde(rename = "avgNodeCount")]
    pub avg_node_count: String,
    #[serde(rename = "runePriceUSD")]
    pub rune_price_usd: String,
    pub pools: Vec<EarningsHistoryItemPool>,
}

#[derive(Debug, Serialize, Default, Clone)]
pub struct LiquidityHistoryItem {
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "endTime")]
    pub end_time: String,
    #[serde(rename = "addLiquidityCount")]
    pub add_liquidity_count: String,
    #[serde(rename = "addLiquidityVolume")]
    pub add_liquidity_volume: String,
    #[serde(rename = "addAssetLiquidityVolume")]
    pub add_asset_liquidity_volume: String,
    #[serde(rename = "addRuneLiquidityVolume")]
    pub add_rune_liquidity_volume: String,
    #[serde(rename = "withdrawCount")]
    pub withdraw_count: String,
    #[serde(rename = "withdrawVolume")]
    pub withdraw_volume: String,
    #[serde(rename = "withdrawAssetVolume")]
    pub withdraw_asset_volume: String,
    #[serde(rename = "withdrawRuneVolume")]
    pub withdraw_rune_volume: String,
    pub net: String,
    #[serde(rename = "runePriceUSD")]
    pub rune_price_usd: String,
}

#[derive(Debug, Serialize, Default, Clone)]
pub struct TvlHistoryItem {
    #[serde(rename = "startTime")]
    pub start_time: String,
    #[serde(rename = "endTime")]
    pub end_time: String,
    #[serde(rename = "totalValuePooled")]
    pub total_value_pooled: String,
    #[serde(rename = "totalValueBonded")]
    pub total_value_bonded: String,
    #[serde(rename = "totalValueLocked")]
    pub total_value_locked: String,
    #[serde(rename = "runePriceUSD")]
    pub rune_price_usd: String,
    #[serde(rename = "poolsDepth")]
    pub pools_depth: Vec<TvlPoolDepth>,
}

#[derive(Debug, Serialize, Default, Clone)]
pub struct TvlPoolDepth {
    pub pool: String,
    #[serde(rename = "totalDepth")]
    pub total_depth: String,
}

#[derive(Debug, Serialize, Default)]
pub struct MemberPool {
    pub pool: String,
    #[serde(rename = "runeAddress")]
    pub rune_address: String,
    #[serde(rename = "assetAddress")]
    pub asset_address: String,
    #[serde(rename = "liquidityUnits")]
    pub liquidity_units: String,
    #[serde(rename = "runeAdded")]
    pub rune_added: String,
    #[serde(rename = "assetAdded")]
    pub asset_added: String,
    #[serde(rename = "runeWithdrawn")]
    pub rune_withdrawn: String,
    #[serde(rename = "assetWithdrawn")]
    pub asset_withdrawn: String,
    #[serde(rename = "runePending")]
    pub rune_pending: String,
    #[serde(rename = "assetPending")]
    pub asset_pending: String,
    #[serde(rename = "dateFirstAdded")]
    pub date_first_added: String,
    #[serde(rename = "dateLastAdded")]
    pub date_last_added: String,
}

#[derive(Debug, Serialize)]
pub struct MemberDetails {
    pub pools: Vec<MemberPool>,
}

#[derive(Debug, Serialize, Default)]
pub struct Coin {
    pub asset: String,
    pub amount: String,
}

#[derive(Debug, Serialize, Default)]
pub struct Transaction {
    pub address: String,
    #[serde(rename = "txID")]
    pub tx_id: String,
    pub coins: Vec<Coin>,
}

#[derive(Debug, Serialize, Default)]
pub struct Action {
    pub date: String,
    pub height: String,
    #[serde(rename = "type")]
    pub action_type: String,
    pub status: String,
    pub pools: Vec<String>,
    #[serde(rename = "in")]
    pub inputs: Vec<Transaction>,
    #[serde(rename = "out")]
    pub outputs: Vec<Transaction>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct Actions {
    pub actions: Vec<Action>,
    pub count: String,
    pub meta: ActionsMeta,
}

#[derive(Debug, Serialize, Default)]
pub struct ActionsMeta {
    #[serde(rename = "nextPageToken")]
    pub next_page_token: String,
    #[serde(rename = "prevPageToken")]
    pub prev_page_token: String,
}

#[derive(Debug, Serialize, Default)]
pub struct Network {
    #[serde(rename = "activeNodeCount")]
    pub active_node_count: String,
    #[serde(rename = "standbyNodeCount")]
    pub standby_node_count: String,
    #[serde(rename = "totalReserve")]
    pub total_reserve: String,
    #[serde(rename = "totalPooledRune")]
    pub total_pooled_rune: String,
    #[serde(rename = "bondingAPY")]
    pub bonding_apy: String,
    #[serde(rename = "liquidityAPY")]
    pub liquidity_apy: String,
    #[serde(rename = "poolActivationCountdown")]
    pub pool_activation_countdown: String,
    #[serde(rename = "nextChurnHeight")]
    pub next_churn_height: String,
    #[serde(rename = "bondMetrics")]
    pub bond_metrics: BondMetrics,
}

#[derive(Debug, Serialize, Default)]
pub struct BondMetrics {
    #[serde(rename = "totalActiveBond")]
    pub total_active_bond: String,
    #[serde(rename = "averageActiveBond")]
    pub average_active_bond: String,
    #[serde(rename = "medianActiveBond")]
    pub median_active_bond: String,
    #[serde(rename = "minimumActiveBond")]
    pub minimum_active_bond: String,
    #[serde(rename = "maximumActiveBond")]
    pub maximum_active_bond: String,
}

#[derive(Debug, Serialize, Default)]
pub struct Stats {
    #[serde(rename = "runePriceUSD")]
    pub rune_price_usd: String,
    #[serde(rename = "switchedRune")]
    pub switched_rune: String,
    #[serde(rename = "runeDepth")]
    pub rune_depth: String,
    #[serde(rename = "swapVolume")]
    pub swap_volume: String,
    #[serde(rename = "swapCount")]
    pub swap_count: String,
    #[serde(rename = "swapCount24h")]
    pub swap_count_24h: String,
    #[serde(rename = "toAssetCount")]
    pub to_asset_count: String,
    #[serde(rename = "toRuneCount")]
    pub to_rune_count: String,
    #[serde(rename = "synthMintCount")]
    pub synth_mint_count: String,
    #[serde(rename = "synthBurnCount")]
    pub synth_burn_count: String,
    #[serde(rename = "dailyActiveUsers")]
    pub daily_active_users: String,
    #[serde(rename = "monthlyActiveUsers")]
    pub monthly_active_users: String,
    #[serde(rename = "uniqueSwapperCount")]
    pub unique_swapper_count: String,
    #[serde(rename = "addLiquidityVolume")]
    pub add_liquidity_volume: String,
    #[serde(rename = "addLiquidityCount")]
    pub add_liquidity_count: String,
    #[serde(rename = "withdrawVolume")]
    pub withdraw_volume: String,
    #[serde(rename = "withdrawCount")]
    pub withdraw_count: String,
    #[serde(rename = "impermanentLossProtectionPaid")]
    pub impermanent_loss_protection_paid: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_serialise_as_strings() {
        // The property clients depend on: a depth past 2^53 must survive as text.
        let item = DepthHistoryItem {
            asset_depth: "9007199254740993".to_string(),
            ..DepthHistoryItem::default()
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(
            json.contains("\"assetDepth\":\"9007199254740993\""),
            "{json}"
        );
    }

    #[test]
    fn field_names_are_camel_case_on_the_wire() {
        let json = serde_json::to_value(DepthHistoryItem::default()).unwrap();
        for key in [
            "startTime",
            "endTime",
            "assetPriceUSD",
            "liquidityUnits",
            "membersCount",
        ] {
            assert!(json.get(key).is_some(), "missing {key} in {json}");
        }
        // And the Rust spellings must not leak.
        for key in ["start_time", "asset_price_usd"] {
            assert!(json.get(key).is_none(), "{key} leaked into the response");
        }
    }

    #[test]
    fn height_and_timestamp_stay_numbers() {
        // HeightTS is the one place the spec uses real JSON numbers.
        let json = serde_json::to_value(HeightTs {
            height: 42,
            timestamp: 7,
        })
        .unwrap();
        assert_eq!(json["height"], 42);
        assert_eq!(json["timestamp"], 7);
    }

    #[test]
    fn history_wraps_intervals_and_meta() {
        let h = History {
            intervals: vec![SwapHistoryItem::default()],
            meta: SwapHistoryItem::default(),
        };
        let json = serde_json::to_value(&h).unwrap();
        assert!(json["intervals"].is_array());
        assert!(json["meta"].is_object());
    }

    #[test]
    fn actions_use_in_and_out_not_the_rust_names() {
        let json = serde_json::to_value(Action::default()).unwrap();
        assert!(json.get("in").is_some());
        assert!(json.get("out").is_some());
        assert!(json.get("type").is_some());
        assert!(json.get("inputs").is_none());
    }
}
