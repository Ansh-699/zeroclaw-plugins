use liquidation_guard::health::{assess, PositionFacts, PriorSnapshotFacts, Thresholds, Tier};

fn thresholds() -> Thresholds {
    Thresholds {
        watch: 0.25,
        warn: 0.15,
        critical: 0.07,
    }
}

fn facts() -> PositionFacts {
    PositionFacts {
        ltv: 0.5,
        liq_ltv: 0.8,
        borrow_usd: 500.0,
        deposit_usd: 1000.0,
        collateral_symbol: "SOL".to_string(),
        debt_symbol: "USDC".to_string(),
        collateral_price: 100.0,
        // Matches collateral_price so tests written before F1 (debt-rise
        // denominated in debt_price, not collateral_price) keep the same
        // expected numbers; `debt_rise_denominated_in_debt_price` below
        // overrides this to prove the two are independent.
        debt_price: 100.0,
        lst_stake_rate: None,
        multi_volatile_collateral: false,
        elevation_group: 0,
        adl_assets: Vec::new(),
        position_value_usd: 1000.0,
        min_full_liquidation_value_usd: Some(2.0),
        borrow_apy: None,
        utilization: None,
    }
}

/// harden F13: percents leaking into these fraction-only modules produce a
/// wildly wrong buffer. liq_ltv=0.799, ltv=0.755 -> buffer ~5.5%, well under
/// the 7% critical threshold -> CRITICAL. If a caller passed 79.9/75.5
/// (percent, not fraction) the buffer would come out totally different.
#[test]
fn fraction_units_regression() {
    let mut f = facts();
    f.liq_ltv = 0.799;
    f.ltv = 0.755;
    let report = assess(&f, None, &thresholds());
    assert!(!report.buffer.is_nan());
    assert!(
        (report.buffer - 0.0550_688).abs() < 1e-4,
        "buffer was {}",
        report.buffer
    );
    assert!(report.buffer < 0.07);
    assert_eq!(report.tier, Tier::Critical);
}

#[test]
fn tier_boundaries_exact() {
    let t = thresholds();
    // liq_ltv = 1.0 so buffer == (1 - ltv), easy to hit exact boundaries.
    let mut f = facts();
    f.liq_ltv = 1.0;

    f.ltv = 1.0 - 0.25; // buffer == watch
    assert_eq!(assess(&f, None, &t).tier, Tier::Ok);

    f.ltv = 1.0 - 0.15; // buffer == warn
    assert_eq!(assess(&f, None, &t).tier, Tier::Watch);

    f.ltv = 1.0 - 0.07; // buffer == critical
    assert_eq!(assess(&f, None, &t).tier, Tier::Warn);

    f.ltv = 1.0 - 0.05; // below critical
    assert_eq!(assess(&f, None, &t).tier, Tier::Critical);
}

#[test]
fn both_forecast_directions() {
    let f = facts(); // collateral_price=100, ltv=0.5, liq_ltv=0.8
    let report = assess(&f, None, &thresholds());
    assert!((report.liq_price_collateral_drop.unwrap() - 62.5).abs() < 1e-9);
    assert!((report.liq_price_debt_rise.unwrap() - 160.0).abs() < 1e-9);
    assert!(report.sol_spot_price.is_none());
}

/// harden F1: debt-rise must be denominated in the debt asset's own price,
/// never the collateral's — the two are unrelated assets (a stablecoin debt
/// against BTC collateral, say).
#[test]
fn debt_rise_denominated_in_debt_price() {
    let mut f = facts(); // collateral_price=100, ltv=0.5, liq_ltv=0.8
    f.debt_price = 1.0; // a stablecoin, wildly different from collateral_price
    let report = assess(&f, None, &thresholds());
    // debt_price * liq_ltv / ltv = 1.0 * 0.8 / 0.5 = 1.6, NOT 160.0 (which
    // is what collateral_price * liq_ltv / ltv would give).
    assert!(
        (report.liq_price_debt_rise.unwrap() - 1.6).abs() < 1e-9,
        "liq_price_debt_rise was {:?}, expected 1.6 (debt_price-denominated)",
        report.liq_price_debt_rise
    );
}

/// The COLLATERAL-drop forecast converts to the SOL level and exposes the
/// matching SOL spot; the DEBT-rise forecast is untouched.
#[test]
fn lst_forecast_converts_collateral_only_and_exposes_sol_spot() {
    let mut f = facts(); // collateral_price=100, debt_price=200, ltv=0.5, liq_ltv=0.8
    f.lst_stake_rate = Some(1.25);
    let report = assess(&f, None, &thresholds());
    assert_eq!(report.sol_spot_price, Some(100.0 / 1.25));
    assert!((report.liq_price_collateral_drop.unwrap() - 62.5 / 1.25).abs() < 1e-9);
}

/// harden DEFECT-1: the collateral's stake rate must NEVER touch the
/// debt-rise forecast. A JitoSOL-collateral / stablecoin-debt position is
/// the common Kamino shape, and dividing a $1.00 stablecoin threshold by a
/// ~1.2 SOL stake rate is dimensionally meaningless.
#[test]
fn lst_stake_rate_never_applied_to_debt_rise() {
    let mut f = facts();
    f.debt_price = 1.0; // stablecoin debt
    f.lst_stake_rate = Some(1.25); // LST collateral
    let report = assess(&f, None, &thresholds());
    // debt_price * liq_ltv / ltv = 1.0 * 0.8 / 0.5 = 1.6 — NOT 1.6 / 1.25.
    assert!(
        (report.liq_price_debt_rise.unwrap() - 1.6).abs() < 1e-9,
        "debt-rise was {:?}, expected 1.6 (undivided by the collateral stake rate)",
        report.liq_price_debt_rise
    );
}

/// The forecast percentage is denomination-invariant: converting the
/// collateral line to the SOL level must not change how far the price has
/// to move, only the units it is quoted in.
#[test]
fn sol_level_conversion_preserves_the_required_move() {
    let mut f = facts();
    let token_level = assess(&f, None, &thresholds());
    let token_move = token_level.liq_price_collateral_drop.unwrap() / f.collateral_price;

    f.lst_stake_rate = Some(1.25);
    let sol_level = assess(&f, None, &thresholds());
    let sol_move = sol_level.liq_price_collateral_drop.unwrap() / sol_level.sol_spot_price.unwrap();

    assert!(
        (token_move - sol_move).abs() < 1e-12,
        "required move changed with denomination: {token_move} vs {sol_move}"
    );
}

#[test]
fn guarded_division_zero_liq_ltv_is_nan_free() {
    let mut f = facts();
    f.liq_ltv = 0.0;
    let report = assess(&f, None, &thresholds());
    assert!(!report.buffer.is_nan());
    assert_eq!(report.buffer, 0.0);
    assert!(report.liq_price_collateral_drop.is_none());
    // ltv is still nonzero here, so debt-rise is well-defined (P*0/ltv = 0),
    // not None -- only liq_ltv == 0 disables the collateral-drop forecast.
    assert!(!report.liq_price_debt_rise.unwrap().is_nan());
    assert_eq!(report.tier, Tier::Critical);
}

#[test]
fn guarded_division_zero_ltv_is_nan_free() {
    let mut f = facts();
    f.ltv = 0.0;
    let report = assess(&f, None, &thresholds());
    assert!(!report.buffer.is_nan());
    assert!(report.liq_price_debt_rise.is_none());
    assert!(report.liq_price_collateral_drop.is_some());
    assert!(!report.liq_price_collateral_drop.unwrap().is_nan());
}

#[test]
fn guarded_division_zero_deposit_usd_is_nan_free() {
    let mut f = facts();
    f.deposit_usd = 0.0;
    let report = assess(&f, None, &thresholds());
    assert!(!report.buffer.is_nan());
    assert!(!report.liq_price_collateral_drop.unwrap().is_nan());
    assert!(!report.liq_price_debt_rise.unwrap().is_nan());
}

fn prior() -> PriorSnapshotFacts {
    PriorSnapshotFacts {
        ltv: 0.45,
        liq_ltv: 0.8,
        collateral_price: 100.0,
        elevation_group: 0,
    }
}

#[test]
fn interest_drift_only_at_flat_prices() {
    let f = facts(); // ltv=0.5, collateral_price=100 (flat vs prior)
    let report = assess(&f, Some(&prior()), &thresholds());
    assert!((report.interest_drift.unwrap() - (0.5 - 0.45)).abs() < 1e-9);
}

#[test]
fn interest_drift_none_when_price_moved() {
    let mut f = facts();
    f.collateral_price = 105.0; // 5% move, well above the 1% flat-price band
    let report = assess(&f, Some(&prior()), &thresholds());
    assert!(report.interest_drift.is_none());
}

#[test]
fn interest_drift_none_without_prior() {
    let f = facts();
    let report = assess(&f, None, &thresholds());
    assert!(report.interest_drift.is_none());
}

#[test]
fn param_alert_fires_on_liq_ltv_change() {
    let f = facts(); // liq_ltv=0.8, prior liq_ltv=0.8 -> no alert by default
    let report = assess(&f, Some(&prior()), &thresholds());
    assert!(report.param_alert.is_none());

    let mut f2 = facts();
    f2.liq_ltv = 0.78;
    let report2 = assess(&f2, Some(&prior()), &thresholds());
    assert!(report2.param_alert.is_some());
}

#[test]
fn param_alert_fires_on_elevation_group_change_independent_of_tier() {
    let mut f = facts();
    f.elevation_group = 3; // prior elevation_group = 0, everything else flat/healthy
    let report = assess(&f, Some(&prior()), &thresholds());
    assert!(report.param_alert.is_some());
    assert_eq!(report.tier, Tier::Ok);
}

#[test]
fn adl_warning_flags_matching_symbol() {
    let mut f = facts();
    f.adl_assets = vec!["USDC".to_string()];
    let report = assess(&f, None, &thresholds());
    assert!(report.adl_warning.is_some());

    let mut f2 = facts();
    f2.adl_assets = vec!["BONK".to_string()];
    let report2 = assess(&f2, None, &thresholds());
    assert!(report2.adl_warning.is_none());
}

#[test]
fn dust_warning_below_threshold() {
    let mut f = facts();
    f.position_value_usd = 1.0;
    f.min_full_liquidation_value_usd = Some(2.0);
    let report = assess(&f, None, &thresholds());
    assert!(report.dust_warning);

    let mut f2 = facts();
    f2.position_value_usd = 3.0;
    f2.min_full_liquidation_value_usd = Some(2.0);
    let report2 = assess(&f2, None, &thresholds());
    assert!(!report2.dust_warning);
}

/// harden F5: a missing dust threshold (payload didn't carry the field)
/// suppresses the warning outright — never a fabricated default, never a
/// false positive from treating "unknown" as "below".
#[test]
fn dust_warning_suppressed_when_threshold_absent() {
    let mut f = facts();
    f.position_value_usd = 0.01; // would trip any real threshold
    f.min_full_liquidation_value_usd = None;
    let report = assess(&f, None, &thresholds());
    assert!(!report.dust_warning);
}

/// harden F2: `borrow_apy`/`utilization` are a pure pass-through onto
/// `HealthReport` — no new math, no fabrication when absent.
#[test]
fn borrow_apy_and_utilization_pass_through() {
    let mut f = facts();
    f.borrow_apy = Some(0.123);
    f.utilization = Some(0.81);
    let report = assess(&f, None, &thresholds());
    assert_eq!(report.borrow_apy, Some(0.123));
    assert_eq!(report.utilization, Some(0.81));

    let f2 = facts(); // both None by default
    let report2 = assess(&f2, None, &thresholds());
    assert_eq!(report2.borrow_apy, None);
    assert_eq!(report2.utilization, None);
}

#[test]
fn correlated_move_assumption_mirrors_input() {
    let mut f = facts();
    f.multi_volatile_collateral = true;
    let report = assess(&f, None, &thresholds());
    assert!(report.correlated_move_assumption);
}
