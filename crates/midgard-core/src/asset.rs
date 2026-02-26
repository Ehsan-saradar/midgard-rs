//! Asset names.
//!
//! THORChain writes an asset as `CHAIN<sep>SYMBOL`, where the separator says what kind of thing
//! you are looking at:
//!
//! ```text
//! BTC.BTC                 native  L1 asset held in a vault
//! BTC/BTC                 synth   claim on the BTC pool
//! BTC~BTC                 trade   trade-account balance
//! BTC-BTC                 secure  IBC-wrapped asset (v3)
//! THOR.RUNE               native  the settlement asset
//! THOR.BTC                derived derived asset, used by lending
//! ETH.USDT-0XDAC17F...    native  with a contract-address suffix on the symbol
//! ```
//!
//! Symbols carry an optional `-ID` suffix; the ticker is everything before it.

use std::fmt;

/// What flavour of asset a name refers to.
///
/// The ordering of the checks in [`CoinType::of`] matters and mirrors THORNode: `X/` contracts
/// and the native THOR tokens are special-cased before the separator rules get a look in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoinType {
    Rune,
    Native,
    Synth,
    Trade,
    Derived,
    Secure,
    Unknown,
}

/// Native RUNE on THORChain.
pub const NATIVE_RUNE: &str = "THOR.RUNE";
/// RUNE on Binance Chain testnet, still referenced by historical events.
pub const RUNE_67C: &str = "BNB.RUNE-67C";
/// RUNE on Binance Chain mainnet, still referenced by historical events.
pub const RUNE_B1A: &str = "BNB.RUNE-B1A";

const NATIVE_TCY: &str = "THOR.TCY";
const NATIVE_RUJI: &str = "THOR.RUJI";
const NATIVE_NAMI: &str = "THOR.NAMI";

/// True for any of the three spellings of RUNE that appear in the event stream.
pub fn is_rune(asset: &str) -> bool {
    matches!(asset, NATIVE_RUNE | RUNE_67C | RUNE_B1A)
}

impl CoinType {
    pub fn of(asset: &str) -> CoinType {
        if is_rune(asset) {
            return CoinType::Rune;
        }
        // These live on THOR. but are ordinary tokens rather than derived assets, so they have
        // to be caught before the `THOR.` prefix rule below.
        if matches!(asset, NATIVE_TCY | NATIVE_RUJI | NATIVE_NAMI) {
            return CoinType::Native;
        }
        let upper = asset.to_ascii_uppercase();
        if upper.starts_with("X/") {
            return CoinType::Native;
        }
        if asset.contains('/') {
            return CoinType::Synth;
        }
        if upper.starts_with("THOR.") {
            return CoinType::Derived;
        }
        if asset.contains('.') {
            return CoinType::Native;
        }
        if asset.contains('~') {
            return CoinType::Trade;
        }
        if asset.contains('-') {
            return CoinType::Secure;
        }
        CoinType::Unknown
    }
}

/// A parsed asset name.
///
/// Parsing never fails: an unrecognisable string is treated as a bare symbol on `THOR`, which is
/// what the Go implementation does and what the rest of the pipeline expects.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Asset {
    pub chain: String,
    pub ticker: String,
    pub symbol: String,
    pub synth: bool,
    pub trade: bool,
}

impl Asset {
    pub fn parse(s: &str) -> Asset {
        let (parts, synth, trade) = if s.contains('/') {
            (s.splitn(2, '/').collect::<Vec<_>>(), true, false)
        } else if s.contains('~') {
            (s.splitn(2, '~').collect::<Vec<_>>(), false, true)
        } else {
            (s.splitn(2, '.').collect::<Vec<_>>(), false, false)
        };

        let (chain, sym) = match parts.as_slice() {
            [only] => ("THOR".to_string(), (*only).to_string()),
            [chain, sym] => (chain.to_ascii_uppercase(), (*sym).to_string()),
            _ => unreachable!("splitn(2) yields one or two parts"),
        };

        let symbol = sym.to_ascii_uppercase();
        let ticker = symbol.split('-').next().unwrap_or("").to_string();

        Asset { chain, ticker, symbol, synth, trade }
    }

    /// `true` for pools whose asset lives on THORChain itself (derived assets, TCY, ...).
    pub fn is_native_chain(&self) -> bool {
        self.chain == "THOR"
    }
}

impl fmt::Display for Asset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sep = if self.synth {
            '/'
        } else if self.trade {
            '~'
        } else {
            '.'
        };
        write!(f, "{}{}{}", self.chain, sep, self.symbol)
    }
}

/// `BTC.BTC` -> `BTC/BTC`. Only the first separator is replaced, because contract addresses in
/// the symbol may contain dots.
pub fn native_pool_to_synth(pool: &str) -> String {
    pool.replacen('.', "/", 1)
}

/// `BTC/BTC` -> `BTC.BTC`.
pub fn synth_pool_to_native(pool: &str) -> String {
    pool.replacen('/', ".", 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_plain_l1_asset() {
        let a = Asset::parse("BTC.BTC");
        assert_eq!(a.chain, "BTC");
        assert_eq!(a.ticker, "BTC");
        assert_eq!(a.symbol, "BTC");
        assert!(!a.synth && !a.trade);
    }

    #[test]
    fn keeps_contract_address_in_symbol_but_not_ticker() {
        let a = Asset::parse("ETH.USDT-0XDAC17F958D2EE523A2206206994597C13D831EC7");
        assert_eq!(a.chain, "ETH");
        assert_eq!(a.ticker, "USDT");
        assert_eq!(a.symbol, "USDT-0XDAC17F958D2EE523A2206206994597C13D831EC7");
    }

    #[test]
    fn lowercase_input_is_normalised() {
        let a = Asset::parse("eth.usdc-0xa0b8");
        assert_eq!(a.chain, "ETH");
        assert_eq!(a.symbol, "USDC-0XA0B8");
        assert_eq!(a.ticker, "USDC");
    }

    #[test]
    fn bare_symbol_defaults_to_thor() {
        let a = Asset::parse("RUNE");
        assert_eq!(a.chain, "THOR");
        assert_eq!(a.symbol, "RUNE");
    }

    #[test]
    fn synth_and_trade_separators_are_recognised() {
        assert!(Asset::parse("BTC/BTC").synth);
        assert!(Asset::parse("BTC~BTC").trade);
    }

    #[test]
    fn display_round_trips() {
        for s in ["BTC.BTC", "BTC/BTC", "BTC~BTC", "ETH.USDT-0XDAC17"] {
            assert_eq!(Asset::parse(s).to_string(), s);
        }
    }

    #[test]
    fn coin_types() {
        assert_eq!(CoinType::of("THOR.RUNE"), CoinType::Rune);
        assert_eq!(CoinType::of("BNB.RUNE-B1A"), CoinType::Rune);
        assert_eq!(CoinType::of("BTC.BTC"), CoinType::Native);
        assert_eq!(CoinType::of("BTC/BTC"), CoinType::Synth);
        assert_eq!(CoinType::of("BTC~BTC"), CoinType::Trade);
        assert_eq!(CoinType::of("BTC-BTC"), CoinType::Secure);
        assert_eq!(CoinType::of("THOR.BTC"), CoinType::Derived);
        assert_eq!(CoinType::of("THOR.TCY"), CoinType::Native);
        assert_eq!(CoinType::of("X/RUJI"), CoinType::Native);
        assert_eq!(CoinType::of("nonsense"), CoinType::Unknown);
    }

    #[test]
    fn pool_name_conversion_only_touches_first_separator() {
        assert_eq!(native_pool_to_synth("ETH.USDT-0X.A"), "ETH/USDT-0X.A");
        assert_eq!(synth_pool_to_native("ETH/USDT-0X.A"), "ETH.USDT-0X.A");
    }
}
