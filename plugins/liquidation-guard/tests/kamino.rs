//! Tests for `kamino.rs` against committed, live-captured fixtures.
//! Offline-deterministic: no network access, no writes into the crate.

use liquidation_guard::kamino::{
    decode_snapshot, encode_snapshot, http_date_to_unix, parse_obligations, parse_prices,
    parse_reserves_metrics, price_is_stale, PriceRow, Snapshot,
};

const OBLIGATIONS: &str = include_str!("fixtures/obligations.json");
const PRICES: &str = include_str!("fixtures/prices.json");
const RESERVES_METRICS: &str = include_str!("fixtures/reserves_metrics.json");

const EXPECTED_OWNER: &str = "AcNSmd5CxwLs21TYUmhWt7CW2v159TdYRkvQxb1iBYRj";

#[test]
fn obligations_parse() {
    let obligations = parse_obligations(OBLIGATIONS).expect("obligations parse");
    assert!(!obligations.is_empty());
    let o = &obligations[0];

    assert_eq!(o.owner, EXPECTED_OWNER);
    assert_eq!(o.referrer, None, "all-zero sentinel referrer maps to None");

    // Fixture has 8 fixed deposit slots and 5 fixed borrow slots; only the
    // non-placeholder (non all-zero-reserve) rows should survive.
    assert_eq!(o.deposits.len(), 2, "placeholder deposit rows filtered");
    assert_eq!(o.borrows.len(), 1, "placeholder borrow rows filtered");
    for row in o.deposits.iter().chain(o.borrows.iter()) {
        assert_ne!(row.reserve, "11111111111111111111111111111111");
        assert!(!row.raw_amount.is_empty());
    }

    // Fractions, not percents.
    assert!(o.ltv > 0.0 && o.ltv < 1.0, "ltv = {}", o.ltv);
    assert!(
        o.liq_ltv > 0.0 && o.liq_ltv < 1.0,
        "liq_ltv = {}",
        o.liq_ltv
    );
    assert!(o.borrow_usd > 0.0);
    assert!(o.deposit_usd > 0.0);

    assert!(!o.obligation.is_empty());
    assert!(!o.market.is_empty());
}

/// harden F5: the dust threshold is parsed straight off the fetched
/// payload (`market.state.minFullLiquidationValueThreshold`, a JSON
/// string like every Kamino numeric) — never a hardcoded default.
#[test]
fn dust_threshold_parsed_from_payload() {
    let obligations = parse_obligations(OBLIGATIONS).expect("obligations parse");
    assert_eq!(obligations[0].min_full_liquidation_value_usd, Some(2.0));
}

/// harden F5: a payload missing the field maps to `None` (dust check
/// suppressed), never a hard parse error and never a silent default.
#[test]
fn missing_dust_threshold_is_none_not_error() {
    let mut value: serde_json::Value = serde_json::from_str(OBLIGATIONS).unwrap();
    value[0]["market"]["state"]
        .as_object_mut()
        .unwrap()
        .remove("minFullLiquidationValueThreshold");
    let body = serde_json::to_string(&value).unwrap();

    let obligations =
        parse_obligations(&body).expect("missing dust threshold must not be a hard error");
    assert_eq!(obligations[0].min_full_liquidation_value_usd, None);
}

/// harden F4: `market.state.autodeleverageEnabled` (a JSON number, `1` on
/// the committed fixture) parses to `true` — the market payload this
/// pipeline already fetches does carry the flag.
#[test]
fn market_adl_enabled_parsed_from_payload() {
    let obligations = parse_obligations(OBLIGATIONS).expect("obligations parse");
    assert!(obligations[0].market_adl_enabled);
}

#[test]
fn prices_parse() {
    let prices = parse_prices(PRICES).expect("prices parse");
    assert_eq!(prices.len(), 58);
    for row in &prices {
        assert!(!row.mint.is_empty());
        assert!(!row.name.is_empty());
        assert!(row.price > 0.0);
        assert!(row.timestamp > 0);
        assert!(row.max_age_s > 0);
    }
}

#[test]
fn metrics_parse() {
    let metrics = parse_reserves_metrics(RESERVES_METRICS).expect("metrics parse");
    assert_eq!(metrics.len(), 58);
    for m in &metrics {
        assert!(!m.reserve.is_empty());
        assert!(!m.mint.is_empty());
        assert!(!m.symbol.is_empty());
        assert!(m.borrow_apy >= 0.0);
        // utilization is Some for every live row (all have nonzero supply)
        // but the type stays Option — guard the divide-by-zero case exists.
        if let Some(u) = m.utilization {
            assert!(u >= 0.0);
        }
    }
}

#[test]
fn snapshot_round_trip() {
    let s = Snapshot {
        v: 1,
        obligation: "HcrU9nyaBFmhNPrxnwXRjreVxdQTZdq2dpvktjsWiS4J".to_string(),
        ltv: 0.7281521318825485,
        liq_ltv: 0.7992550392596365,
        collateral_price: 151.4,
        elevation_group: 0,
        taken_unix: 1_784_388_667,
    };
    let encoded = encode_snapshot(&s);
    let decoded = decode_snapshot(&encoded).expect("round trip");
    assert_eq!(decoded.v, s.v);
    assert_eq!(decoded.obligation, s.obligation);
    assert_eq!(decoded.ltv, s.ltv);
    assert_eq!(decoded.liq_ltv, s.liq_ltv);
    assert_eq!(decoded.collateral_price, s.collateral_price);
    assert_eq!(decoded.elevation_group, s.elevation_group);
    assert_eq!(decoded.taken_unix, s.taken_unix);
}

/// harden F6: an old-format snapshot (predates the `obligation` field)
/// fails to deserialize — a required field is missing — which already
/// degrades to `None` via `decode_snapshot`'s any-failure-is-None contract;
/// no version bump needed.
#[test]
fn old_format_snapshot_missing_obligation_is_none() {
    let old = serde_json::json!({
        "v": 1,
        "ltv": 0.5,
        "liq_ltv": 0.8,
        "collateral_price": 100.0,
        "elevation_group": 0,
        "taken_unix": 1,
    })
    .to_string();
    assert!(decode_snapshot(&old).is_none());
}

#[test]
fn decode_garbage_snapshot_is_none() {
    assert!(decode_snapshot("garbage").is_none());
    assert!(decode_snapshot("").is_none());
    assert!(
        decode_snapshot("{\"v\":1}").is_none(),
        "missing required fields"
    );
}

#[test]
fn missing_required_field_names_it() {
    let mut value: serde_json::Value = serde_json::from_str(OBLIGATIONS).unwrap();
    value[0]["state"].as_object_mut().unwrap().remove("owner");
    let body = serde_json::to_string(&value).unwrap();

    let err = parse_obligations(&body).expect_err("missing owner must error");
    assert!(err.contains("owner"), "error should name the field: {err}");
}

#[test]
fn http_date_to_unix_known_pair() {
    // Verified independently (Python `calendar.timegm`) against the RFC-1123
    // example from the issue spec.
    assert_eq!(
        http_date_to_unix("Sat, 18 Jul 2026 15:31:07 GMT").unwrap(),
        1_784_388_667
    );
    // Cross-checked against this fixture set's own capture-time Date header.
    assert_eq!(
        http_date_to_unix("Sun, 19 Jul 2026 06:53:34 GMT").unwrap(),
        1_784_444_014
    );
}

#[test]
fn http_date_to_unix_rejects_malformed() {
    assert!(http_date_to_unix("not a date").is_err());
    assert!(http_date_to_unix("Sat, 18 Xyz 2026 15:31:07 GMT").is_err());
    assert!(http_date_to_unix("Sat, 18 Jul 2026 15:31:07 UTC").is_err());
}

fn price(timestamp: i64, max_age_s: i64) -> PriceRow {
    PriceRow {
        mint: "mint".into(),
        name: "TOK".into(),
        price: 1.0,
        timestamp,
        max_age_s,
    }
}

#[test]
fn stale_prices_flagged() {
    // now is well past timestamp + max_age_s.
    let row = price(1_784_000_000, 120);
    let now = 1_784_000_300; // 300s later, max age 120s
    assert!(price_is_stale(now, &row));
}

#[test]
fn fresh_prices_not_flagged() {
    // now - timestamp is within max_age_s.
    let row = price(1_784_000_000, 120);
    let now = 1_784_000_050; // 50s later, max age 120s
    assert!(!price_is_stale(now, &row));

    // Real fixture-derived case: first prices row vs. this fixture set's
    // own capture-time Date header (55s old, 120s max age).
    let prices = parse_prices(PRICES).expect("prices parse");
    let now = http_date_to_unix("Sun, 19 Jul 2026 06:53:34 GMT").unwrap();
    assert!(!price_is_stale(now, &prices[0]));
}
