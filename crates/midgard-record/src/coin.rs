//! Coin strings.
//!
//! THORChain writes amounts as `"<e8> <ASSET>"` and lists of them comma-separated:
//!
//! ```text
//! 250000 THOR.TCY
//! 79693 THOR.RUNE
//! 100 BTC.BTC, 5000 THOR.RUNE
//! ```
//!
//! The amount is always an integer in e8; THORNode has already normalised each chain's native
//! precision. An empty string is a legitimate value meaning "no coins", not a parse failure.

use midgard_core::asset::is_rune;

/// One `amount asset` pair.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Coin {
    pub asset: String,
    pub amount_e8: i64,
}

impl Coin {
    pub fn is_rune(&self) -> bool {
        is_rune(&self.asset)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CoinError {
    #[error("coin {0:?} is not '<amount> <asset>'")]
    Shape(String),

    #[error("coin {0:?} has an unparseable amount")]
    Amount(String),
}

/// Parse a single `"<amount> <ASSET>"`.
pub fn parse_coin(s: &str) -> Result<Coin, CoinError> {
    let s = s.trim();
    let (amount, asset) = s
        .split_once(char::is_whitespace)
        .ok_or_else(|| CoinError::Shape(s.to_string()))?;

    let asset = asset.trim();
    if asset.is_empty() {
        return Err(CoinError::Shape(s.to_string()));
    }

    let amount_e8 = amount
        .trim()
        .parse::<i64>()
        .map_err(|_| CoinError::Amount(s.to_string()))?;

    Ok(Coin {
        asset: asset.to_string(),
        amount_e8,
    })
}

/// Parse a comma-separated list. An empty or whitespace-only input yields an empty list.
pub fn parse_coins(s: &str) -> Result<Vec<Coin>, CoinError> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(Vec::new());
    }
    s.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(parse_coin)
        .collect()
}

/// Parse a Cosmos SDK coin: `"<amount><denom>"`, with no separator.
///
/// THORChain speaks two coin dialects and they are not interchangeable. Its own events use
/// `"250000 THOR.TCY"`; the `transfer` events that come from the Cosmos bank module use the SDK's
/// format instead:
///
/// ```text
/// 35300eth-usdc-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48
/// 1000000rune
/// ```
///
/// Denoms are lowercase on the wire and upper-cased here to match the asset spelling used
/// everywhere else, with `rune` mapping to `THOR.RUNE` rather than to `RUNE`.
pub fn parse_cosmos_coin(s: &str) -> Result<Coin, CoinError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(CoinError::Shape(s.to_string()));
    }

    // The amount is the leading run of digits; everything after it is the denom.
    let split = s
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| CoinError::Shape(s.to_string()))?;
    if split == 0 {
        return Err(CoinError::Amount(s.to_string()));
    }

    let amount_e8 = s[..split]
        .parse::<i64>()
        .map_err(|_| CoinError::Amount(s.to_string()))?;

    // Cosmos denoms match [a-zA-Z][a-zA-Z0-9/:._-]{2,127} and so never contain whitespace.
    // Rejecting it rather than trimming keeps the two dialects distinguishable: a THORChain-style
    // "250000 THOR.TCY" would otherwise parse here and quietly produce the right answer, which
    // is precisely how the transfer decoding came to be pointed at the wrong parser in the first
    // place. Upstream trims; being stricter costs nothing because no real denom has a space.
    let denom = &s[split..];
    if denom.is_empty() || denom.chars().any(char::is_whitespace) {
        return Err(CoinError::Shape(s.to_string()));
    }

    // Only `rune` gets mapped to a chain-qualified name; everything else is upper-cased as-is.
    // That is what upstream does, and it means the native THOR.TCY token appears in transfers as
    // plain "TCY" rather than "THOR.TCY". It looks like an inconsistency and arguably is one, but
    // it is the spelling clients already receive, so this port keeps it rather than quietly
    // diverging on the wire.
    let asset = if denom == "rune" {
        midgard_core::asset::NATIVE_RUNE.to_string()
    } else {
        denom.to_ascii_uppercase()
    };

    Ok(Coin { asset, amount_e8 })
}

/// Parse a comma-separated list of Cosmos coins: `"51070btc-btc,1736937eth-eth"`.
///
/// A single bank transfer can move several denominations at once, and treating the whole string
/// as one coin yields an asset named `BTC-BTC,1736937ETH-ETH` — nonsense that inserts cleanly
/// and is only visible if someone looks.
pub fn parse_cosmos_coins(s: &str) -> Result<Vec<Coin>, CoinError> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(Vec::new());
    }
    s.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(parse_cosmos_coin)
        .collect()
}

/// Split a list into its RUNE total and the first non-RUNE coin.
///
/// This is the shape almost every liquidity event wants: a deposit is "some RUNE and some of the
/// pool's asset", and the caller invariably needs those two separately.
pub fn split_rune(coins: &[Coin]) -> (i64, Option<&Coin>) {
    let rune = coins
        .iter()
        .filter(|c| c.is_rune())
        .map(|c| c.amount_e8)
        .sum();
    let asset = coins.iter().find(|c| !c.is_rune());
    (rune, asset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_single_coin() {
        assert_eq!(
            parse_coin("250000 THOR.TCY").unwrap(),
            Coin {
                asset: "THOR.TCY".to_string(),
                amount_e8: 250_000
            }
        );
    }

    #[test]
    fn tolerates_extra_whitespace() {
        assert_eq!(parse_coin("  42   BTC.BTC  ").unwrap().amount_e8, 42);
        assert_eq!(parse_coin("  42   BTC.BTC  ").unwrap().asset, "BTC.BTC");
    }

    #[test]
    fn zero_is_a_valid_amount() {
        // Common on fee and swap events; must not be confused with absent.
        assert_eq!(parse_coin("0 THOR.RUNE").unwrap().amount_e8, 0);
    }

    #[test]
    fn parses_a_list() {
        let coins = parse_coins("100 BTC.BTC, 5000 THOR.RUNE").unwrap();
        assert_eq!(coins.len(), 2);
        assert_eq!(coins[0].asset, "BTC.BTC");
        assert_eq!(coins[1].amount_e8, 5_000);
    }

    #[test]
    fn an_empty_list_is_not_an_error() {
        assert_eq!(parse_coins("").unwrap(), vec![]);
        assert_eq!(parse_coins("   ").unwrap(), vec![]);
    }

    #[test]
    fn trailing_separators_are_ignored() {
        assert_eq!(parse_coins("100 BTC.BTC,").unwrap().len(), 1);
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(matches!(parse_coin("nonsense"), Err(CoinError::Shape(_))));
        assert!(matches!(parse_coin("100"), Err(CoinError::Shape(_))));
        assert!(matches!(
            parse_coin("abc BTC.BTC"),
            Err(CoinError::Amount(_))
        ));
        assert!(matches!(parse_coin("100 "), Err(CoinError::Shape(_))));
    }

    #[test]
    fn assets_with_contract_addresses_survive_intact() {
        let c = parse_coin("1 ETH.USDT-0XDAC17F958D2EE523A2206206994597C13D831EC7").unwrap();
        assert_eq!(
            c.asset,
            "ETH.USDT-0XDAC17F958D2EE523A2206206994597C13D831EC7"
        );
    }

    #[test]
    fn parses_a_real_cosmos_transfer_amount() {
        // Verbatim from a mainnet transfer event.
        let c =
            parse_cosmos_coin("35300eth-usdc-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48").unwrap();
        assert_eq!(c.amount_e8, 35_300);
        assert_eq!(
            c.asset,
            "ETH-USDC-0XA0B86991C6218B36C1D19D4A2E9EB0CE3606EB48"
        );
    }

    #[test]
    fn the_rune_denom_becomes_the_native_asset_name() {
        // "rune" upper-cased would be "RUNE", which matches no pool and is not what the rest of
        // the pipeline calls it.
        let c = parse_cosmos_coin("1000000rune").unwrap();
        assert_eq!(c.asset, "THOR.RUNE");
        assert_eq!(c.amount_e8, 1_000_000);
        assert!(c.is_rune());
    }

    #[test]
    fn cosmos_amounts_of_zero_are_valid() {
        assert_eq!(parse_cosmos_coin("0rune").unwrap().amount_e8, 0);
    }

    #[test]
    fn rejects_malformed_cosmos_coins() {
        for bad in ["", "rune", "123", "  ", "-5rune"] {
            assert!(parse_cosmos_coin(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn parses_a_multi_denomination_transfer() {
        // Observed on mainnet. Parsed as a single coin this yields an asset named
        // "BTC-BTC,1736937ETH-ETH", which is nonsense that inserts without complaint.
        let coins = parse_cosmos_coins("51070btc-btc,1736937eth-eth").unwrap();
        assert_eq!(coins.len(), 2);
        assert_eq!(
            coins[0],
            Coin {
                asset: "BTC-BTC".to_string(),
                amount_e8: 51_070
            }
        );
        assert_eq!(
            coins[1],
            Coin {
                asset: "ETH-ETH".to_string(),
                amount_e8: 1_736_937
            }
        );
    }

    #[test]
    fn a_single_cosmos_coin_is_a_one_element_list() {
        let coins = parse_cosmos_coins("1000000rune").unwrap();
        assert_eq!(coins.len(), 1);
        assert_eq!(coins[0].asset, "THOR.RUNE");
    }

    #[test]
    fn an_empty_cosmos_list_is_not_an_error() {
        assert_eq!(parse_cosmos_coins("").unwrap(), vec![]);
    }

    #[test]
    fn the_two_coin_dialects_do_not_accept_each_others_input() {
        // The bug that motivated this: transfer events were being run through parse_coin, which
        // rejects every one of them, so the whole table stayed empty.
        assert!(parse_coin("1000000rune").is_err());
        assert!(parse_cosmos_coin("250000 THOR.TCY").is_err());
    }

    #[test]
    fn splitting_rune_from_the_asset_side() {
        let coins = parse_coins("100 BTC.BTC, 5000 THOR.RUNE").unwrap();
        let (rune, asset) = split_rune(&coins);
        assert_eq!(rune, 5_000);
        assert_eq!(asset.unwrap().asset, "BTC.BTC");
    }

    #[test]
    fn splitting_recognises_the_legacy_rune_spellings() {
        // Historical events reference BNB-chain RUNE, and treating those as the asset side would
        // silently mis-attribute every pre-migration deposit.
        let coins = parse_coins("100 BTC.BTC, 1 BNB.RUNE-B1A").unwrap();
        let (rune, asset) = split_rune(&coins);
        assert_eq!(rune, 1);
        assert_eq!(asset.unwrap().asset, "BTC.BTC");
    }

    #[test]
    fn splitting_an_asset_only_list() {
        let coins = parse_coins("100 BTC.BTC").unwrap();
        let (rune, asset) = split_rune(&coins);
        assert_eq!(rune, 0);
        assert!(asset.is_some());
    }
}
