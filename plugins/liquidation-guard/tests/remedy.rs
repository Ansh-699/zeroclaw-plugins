use liquidation_guard::remedy::{rank, RemedyInput, RemedyKind};

fn input() -> RemedyInput {
    RemedyInput {
        borrow_usd: 800.0,
        deposit_usd: 1000.0,
        liq_ltv: 0.8,
        watch: 0.25,
        debt_symbol: "USDC".to_string(),
        debt_price: 1.0,
        collateral_symbol: "SOL".to_string(),
        collateral_price: 1.0,
        max_repay_ui: None,
        collateral_is_falling: false,
    }
}

/// v1 has no grace period: remedies restore to the WATCH boundary exactly,
/// never "just under the line", for both directions.
#[test]
fn remedies_restore_to_watch_boundary() {
    let remedies = rank(&input());
    assert_eq!(remedies.len(), 2);

    let repay = &remedies[0];
    assert_eq!(repay.kind, RemedyKind::Repay);
    assert!((repay.ui_amount - 200.0).abs() < 1e-9); // Delta = B - t*D = 800 - 0.6*1000
    assert!((repay.resulting_buffer - 0.25).abs() < 1e-9);
    assert!(!repay.capped_by_max_repay);

    let deposit = &remedies[1];
    assert_eq!(deposit.kind, RemedyKind::Deposit);
    assert!((deposit.ui_amount - (800.0 / 0.6 - 1000.0)).abs() < 1e-6); // Delta = B/t - D
    assert!((deposit.resulting_buffer - 0.25).abs() < 1e-9);
}

#[test]
fn repay_is_always_ranked_first() {
    let remedies = rank(&input());
    assert_eq!(remedies[0].kind, RemedyKind::Repay);
    assert_eq!(remedies[1].kind, RemedyKind::Deposit);
}

#[test]
fn repay_is_ranked_first_even_when_collateral_is_falling() {
    let mut i = input();
    i.collateral_is_falling = true;
    let remedies = rank(&i);
    assert_eq!(remedies[0].kind, RemedyKind::Repay);
    assert_eq!(remedies[1].kind, RemedyKind::Deposit);
}

#[test]
fn capped_repay_reports_the_capped_outcome_not_uncapped() {
    let mut i = input();
    i.max_repay_ui = Some(100.0); // uncapped would be 200.0
    let remedies = rank(&i);
    let repay = &remedies[0];
    assert!(repay.capped_by_max_repay);
    assert!((repay.ui_amount - 100.0).abs() < 1e-9);
    // resulting borrow = 800 - 100 = 700, ltv = 0.7, buffer = (0.8-0.7)/0.8 = 0.125
    assert!((repay.resulting_ltv - 0.7).abs() < 1e-9);
    assert!((repay.resulting_buffer - 0.125).abs() < 1e-9);
    // must not equal the uncapped restore-to-watch outcome
    assert!((repay.resulting_buffer - 0.25).abs() > 1e-6);
}

#[test]
fn empty_when_already_at_or_above_watch_buffer() {
    let mut i = input();
    i.borrow_usd = 500.0; // ltv = 0.5, well above the 0.6 target -> Delta <= 0
    assert!(rank(&i).is_empty());
}

#[test]
fn no_negative_remedy_at_exact_watch_boundary() {
    let mut i = input();
    i.borrow_usd = 600.0; // ltv == t exactly -> Delta == 0
    assert!(rank(&i).is_empty());
}

#[test]
fn all_outputs_are_nan_free_on_degenerate_input() {
    let mut i = input();
    i.deposit_usd = 0.0;
    i.debt_price = 0.0;
    i.collateral_price = 0.0;
    for r in rank(&i) {
        assert!(!r.ui_amount.is_nan());
        assert!(!r.resulting_ltv.is_nan());
        assert!(!r.resulting_buffer.is_nan());
        assert!(!r.needs_balance_ui.is_nan());
    }
}
