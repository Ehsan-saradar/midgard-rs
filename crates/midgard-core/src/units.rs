//! Fixed-point amounts and the string formatting the `/v2` API uses.
//!
//! THORNode normalises every chain's native precision to 8 decimals before it emits events, so
//! Midgard only ever deals in `e8` integers regardless of whether the underlying chain uses 8
//! (BTC) or 18 (ETH) decimals. The native precision is tracked separately, purely so clients can
//! render amounts, and is not used in any arithmetic here.
//!
//! Every numeric field in the `/v2` responses is a JSON *string*. That is not an accident:
//! amounts routinely exceed 2^53 and would lose precision in a JavaScript client if they were
//! sent as JSON numbers. [`int_str`] and [`float_str`] produce byte-identical output to the Go
//! implementation's `util.IntStr` / `util.FloatStr`.

/// One whole unit, in the fixed-point representation used throughout.
pub const E8: i64 = 100_000_000;

/// Sentinel for "we have not observed this pool's native precision".
pub const UNKNOWN_DECIMALS: i64 = -1;

/// Render an integer the way the API does.
pub fn int_str(v: i64) -> String {
    v.to_string()
}

/// Render a float the way the API does.
///
/// Go's `strconv.FormatFloat(v, 'f', -1, 64)` gives the shortest decimal that round-trips, never
/// in exponent form. Rust's `Display` for `f64` has the same two properties, so the only thing
/// needing special handling is the non-finite set, where the two languages disagree on spelling.
pub fn float_str(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 {
            "+Inf".to_string()
        } else {
            "-Inf".to_string()
        };
    }
    let s = v.to_string();
    // Rust prints -0 as "-0"; Go prints it as "-0" too, but 1e21 and up switch to exponent form
    // in Rust's Display while Go's 'f' verb does not. Amounts that large do not occur in
    // practice (500M RUNE at e8 is 5e16), so this is a guard rather than a hot path.
    if s.contains('e') || s.contains('E') {
        return format!("{v:.0}");
    }
    s
}

/// `a / b` with a zero denominator yielding zero rather than NaN.
///
/// Most ratios in the API (APR, slip, price) have a denominator that is legitimately zero for
/// empty pools or empty time buckets, and clients expect `"0"` there, not `"NaN"`.
pub fn ratio(a: f64, b: f64) -> f64 {
    if b == 0.0 {
        0.0
    } else {
        a / b
    }
}

/// Price of a pool's asset denominated in RUNE.
///
/// Zero-depth pools have no meaningful price; upstream returns 0 for them and so do we.
pub fn asset_price(asset_e8: i64, rune_e8: i64) -> f64 {
    if asset_e8 == 0 {
        0.0
    } else {
        rune_e8 as f64 / asset_e8 as f64
    }
}

/// Liquidity Unit Value Index: `sqrt(asset * rune) / units`.
///
/// The geometric mean of the two sides divided by the units outstanding. Because a pool is
/// rebalanced to equal value on both sides, this grows only when the pool earns, which makes it
/// the basis for the LP yield figures.
pub fn luvi(asset_e8: i64, rune_e8: i64, units: i64) -> f64 {
    if units <= 0 {
        return 0.0;
    }
    ((asset_e8 as f64) * (rune_e8 as f64)).sqrt() / units as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_render_plainly() {
        assert_eq!(int_str(0), "0");
        assert_eq!(int_str(-1), "-1");
        assert_eq!(int_str(i64::MAX), "9223372036854775807");
    }

    #[test]
    fn floats_are_shortest_round_trip_without_exponent() {
        assert_eq!(float_str(0.0), "0");
        assert_eq!(float_str(1.5), "1.5");
        assert_eq!(float_str(0.1), "0.1");
        assert_eq!(float_str(1.0 / 3.0), "0.3333333333333333");
        assert!(!float_str(1e30).contains('e'));
    }

    #[test]
    fn non_finite_uses_go_spelling() {
        assert_eq!(float_str(f64::NAN), "NaN");
        assert_eq!(float_str(f64::INFINITY), "+Inf");
        assert_eq!(float_str(f64::NEG_INFINITY), "-Inf");
    }

    #[test]
    fn ratios_do_not_produce_nan() {
        assert_eq!(ratio(1.0, 0.0), 0.0);
        assert_eq!(ratio(1.0, 4.0), 0.25);
    }

    #[test]
    fn price_of_an_empty_pool_is_zero() {
        assert_eq!(asset_price(0, 100), 0.0);
        assert_eq!(asset_price(2 * E8, 8 * E8), 4.0);
    }

    #[test]
    fn luvi_is_the_geometric_mean_over_units() {
        assert_eq!(luvi(0, 0, 0), 0.0);
        assert_eq!(luvi(4, 9, 3), 2.0);
    }
}
