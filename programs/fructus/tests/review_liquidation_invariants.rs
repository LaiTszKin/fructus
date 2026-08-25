//! Independent review-agent regression suite for the collateral-tracking
//! invariant of the liquidation engine (issue #8).
//!
//! The source documents `Position.collateral` as:
//!
//! > "Reserved margin for this position, in USDC microunits: always equals
//! > `margin_required(notional, initial_margin_bps)`."
//!
//! (`programs/fructus/src/state.rs`.) The `apply_open_fills` / `apply_close_fills`
//! adapters maintain exactly that invariant (`position.collateral` is recomputed
//! from the new notional at `initial_margin_bps` on every open/close). This suite
//! pins the SAME invariant across the `liquidate` path: after a partial or full
//! liquidation, the surviving position's collateral must equal
//! `margin_required(surviving_notional, initial_margin_bps)` — and, in
//! particular, a **fully** liquidated (closed, `notional == 0`) position must
//! retain **zero** collateral.
//!
//! NOTE: this is the REVIEW agent's suite. It asserts the invariant documented in
//! `state.rs`, NOT the `remaining`-collateral behavior described in
//! docs/modules/liquidation.md. If it fails, the `liquidate` adapter decouples
//! `position.collateral` from `margin_required(notional)`.

use fructus::liquidation::apply_liquidation;
use fructus::positions::margin_required;
use proptest::prelude::*;

const PENALTY_BPS: u16 = 500; // LIQUIDATION_PENALTY_BPS

/// The invariant: the position collateral surviving a liquidation must be exactly
/// `margin_required(surviving_notional, initial_margin_bps)`. For a full
/// liquidation (`amount == notional`) this means the closed position holds zero
/// collateral (`margin_required(0, _) == Some(0)`).
fn surviving_collateral_matches_invariant(
    notional: u64,
    amount: u64,
    initial_bps: u16,
    maintenance_bps: u16,
) -> bool {
    // The position is backed at the INITIAL margin ratio (state.rs invariant).
    let Some(position_collateral) = margin_required(notional, initial_bps) else {
        return true; // unreachable on the validated domain
    };
    let Ok((remaining, _reward)) = apply_liquidation(
        position_collateral,
        notional,
        amount,
        initial_bps,
        maintenance_bps,
        PENALTY_BPS,
    ) else {
        return true; // invalid amount -> not reached in our generator
    };
    let surviving_notional = notional - amount;
    margin_required(surviving_notional, initial_bps)
        .map(|expected| remaining == expected)
        .unwrap_or(false)
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(10_000))]

    // FULL liquidation: a fully-closed position (notional -> 0) must release ALL
    // of its collateral (the remaining collateral is exactly
    // margin_required(0) == 0), not retain a residual.
    #[test]
    fn full_liquidation_releases_all_collateral(
        (notional, initial_bps, maintenance_bps) in
            (1u64..1_000_000_000u64, 2u16..=10_000u16)
                .prop_flat_map(|(n, ib)| (proptest::prelude::Just(n), proptest::prelude::Just(ib), 1u16..ib))
    ) {
        // maintenance is generated strictly below initial (validated at
        // initialize_market), so no on-chain-unreachable pairs are produced.
        prop_assert!(
            surviving_collateral_matches_invariant(notional, notional, initial_bps, maintenance_bps),
            "a FULLY liquidated (closed) position must retain ZERO collateral, but \
             apply_liquidation left a residual != margin_required(0) == 0"
        );
    }

    // PARTIAL liquidation: the surviving collateral must equal
    // margin_required(notional - amount, initial_bps) — the surviving exposure
    // must be backed at the initial margin ratio, exactly as close_position.
    #[test]
    fn partial_liquidation_backs_surviving_exposure_at_initial_margin(
        (notional, amount, initial_bps, maintenance_bps) in
            (2u64..1_000_000_000u64, 1u64..1_000_000_000u64, 2u16..=10_000u16)
                .prop_flat_map(|(n, a, ib)| (proptest::prelude::Just(n), proptest::prelude::Just(a), proptest::prelude::Just(ib), 1u16..ib))
    ) {
        let amount = amount.min(notional);
        prop_assert!(
            surviving_collateral_matches_invariant(notional, amount, initial_bps, maintenance_bps),
            "after a partial liquidation the surviving position collateral must equal \
             margin_required(notional-amount, initial_bps)"
        );
    }
}

/// Deterministic minimal witness: a fully-liquidated, initially-backed position
/// retains non-zero collateral even though `notional` is now zero, violating the
/// documented `collateral == margin_required(notional)` invariant.
#[test]
fn full_liquidation_zero_notional_still_holds_collateral_witness() {
    // N=100, initial 10% (collateral = 10), maintenance 5% (release = 5),
    // penalty 5% of release (reward = 1): apply_liquidation returns
    // remaining = 10 - 5 - 1 = 4, but a closed (notional == 0) position must
    // hold margin_required(0, _) == 0.
    let notional = 100u64;
    let initial_bps = 1_000u16;
    let maintenance_bps = 500u16;
    let position_collateral = margin_required(notional, initial_bps).expect("total");
    let (remaining, reward) = apply_liquidation(
        position_collateral,
        notional,
        notional,
        initial_bps,
        maintenance_bps,
        PENALTY_BPS,
    )
    .expect("valid full liquidation");

    // The correct (documented) invariant: a fully-closed position holds zero
    // collateral.
    assert_eq!(
        remaining,
        margin_required(0, initial_bps).expect("total"),
        "a fully liquidated (notional == 0) position must release ALL collateral; \
         it retained {remaining} and gave {reward} to the liquidator"
    );
    // And the surviving-collateral invariant at the general level.
    assert_eq!(
        remaining,
        margin_required(notional - notional, initial_bps).expect("total"),
        "surviving collateral must be margin_required(surviving_notional)"
    );
}
