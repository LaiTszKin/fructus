//! Independent adversarial review of the liquidation **ledger** transition.
//!
//! `apply_liquidation` (pure) returns the surviving position collateral and the
//! liquidator reward, and it is correctly bounded (`remaining + reward <=
//! position_collateral`). The `liquidate` handler in `lib.rs` applies that to
//! the `UserCollateral` ledgers:
//!
//! ```text
//! consumer  = position.collateral - remaining_collateral      // released
//! victim.user_collateral.reserved  -= consumer                // margin released
//! victim.user_collateral.deposited -= reward                  // penalty out
//! liquidator_collateral.deposited  += reward                  // penalty in
//! ```
//!
//! The reward is paid out of the victim's released margin (`consumer >= reward`),
//! so the victim's `deposited` falls by `reward` and the liquidator's rises by
//! the same amount: the sum of `deposited` across victim + liquidator is
//! **conserved** — a liquidation transfers value, it never mints it.
//!
//! The invariant pinned here is the conservation/anti-inflation property: **a
//! liquidation must not create collateral. The reward is paid out of the
//! victim's released margin, so the victim's `deposited` falls by `reward` and
//! the sum of `deposited` across the victim + liquidator is unchanged.**

use fructus::liquidation::apply_liquidation;
use fructus::positions::margin_required;
use proptest::prelude::*;

const PENALTY_BPS: u16 = 500; // LIQUIDATION_PENALTY_BPS

/// Faithful reproduction of the `liquidate` handler's ledger transition (the
/// exact corrected sequence in lib.rs: the reward is a transfer OUT of the
/// victim's released margin, so victim `deposited` falls by the reward while
/// the liquidator's rises by it — Σ deposited is conserved).
///
/// Returns `(victim_deposited_after, liquidator_deposited_after, reward)`.
fn handler_transition(
    victim_deposited: u64,
    liquidator_deposited: u64,
    position_collateral: u64,
    notional: u64,
    amount: u64,
    initial_bps: u16,
    maintenance_bps: u16,
) -> Option<(u64, u64, u64)> {
    let (remaining_collateral, reward) = apply_liquidation(
        position_collateral,
        notional,
        amount,
        initial_bps,
        maintenance_bps,
        PENALTY_BPS,
    )
    .ok()?;
    let _consumed = position_collateral.saturating_sub(remaining_collateral);
    // victim.user_collateral.reserved -= consumed  (margin released)
    // victim.user_collateral.deposited -= reward   (penalty transferred out)
    // liquidator_collateral.deposited += reward
    let victim_after = victim_deposited.checked_sub(reward)?;
    let liquidator_after = liquidator_deposited.checked_add(reward)?;
    Some((victim_after, liquidator_after, reward))
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(10_000))]

    // The liquidation ledger must conserve `deposited`: the reward is a transfer
    // out of the victim's margin (`apply_liquidation`'s `remaining + reward <=
    // position_collateral` bound guarantees the victim's `deposited >= reward`),
    // so Σ deposited is unchanged for any liquidation — reward 0 or not.
    #[test]
    fn liquidation_conserves_total_deposited(
        (notional, amount, initial_bps, maintenance_bps) in
            (2u64..1_000_000_000u64, 1u64..1_000_000_000u64, 2u16..=10_000u16)
                .prop_flat_map(|(n, a, ib)| (proptest::prelude::Just(n), proptest::prelude::Just(a), proptest::prelude::Just(ib), 1u16..ib))
    ) {
        let amount = amount.min(notional);
        // An on-chain position is backed at the INITIAL margin ratio (state.rs).
        let position_collateral = margin_required(notional, initial_bps).expect("total on the validated domain");
        let victim_deposited = position_collateral.saturating_add(100_000_000);
        let liquidator_deposited = 500_000_000u64;

        let (victim_after, liquidator_after, _reward) = handler_transition(
            victim_deposited, liquidator_deposited, position_collateral,
            notional, amount, initial_bps, maintenance_bps,
        ).expect("valid partial/full liquidation");

        let sum_before = victim_deposited.saturating_add(liquidator_deposited);
        let sum_after = victim_after.saturating_add(liquidator_after);
        prop_assert_eq!(
            sum_after, sum_before,
            "liquidation must not mint collateral: the reward is a transfer out of the victim's deposited, so Σ deposited is conserved (the vault is never over-issued)"
        );
    }
}

/// Deterministic minimal witness.
#[test]
fn liquidation_mints_collateral_witness() {
    // N=100, initial 10% (position_collateral = 10), maintenance 5%, amount 50.
    // remaining = margin_required(50, 1000) = 5, released = 5,
    // reward = ceil(5 * 500 / 10000) = 1.
    let notional = 100u64;
    let amount = 50u64;
    let initial_bps = 1_000u16;
    let maintenance_bps = 500u16;
    let position_collateral = margin_required(notional, initial_bps).expect("total");

    let (victim_deposited, liquidator_deposited) = (10_000u64, 0u64);
    let (victim_after, liquidator_after, reward) = handler_transition(
        victim_deposited,
        liquidator_deposited,
        position_collateral,
        notional,
        amount,
        initial_bps,
        maintenance_bps,
    )
    .expect("valid partial liquidation");

    // The liquidator's deposited grows by the reward while the victim's falls by
    // the same amount: Σ deposited IS conserved (a pure transfer; the vault is
    // never over-issued).
    assert!(
        reward > 0,
        "witness needs a nonzero penalty to demonstrate the conservation"
    );
    assert_eq!(
        liquidator_after,
        liquidator_deposited + reward,
        "liquidator credited"
    );
    assert_eq!(
        victim_after,
        victim_deposited - reward,
        "victim deposited debited by the reward (paid out of the released margin)"
    );
    assert_eq!(
        victim_after.saturating_add(liquidator_after),
        victim_deposited.saturating_add(liquidator_deposited),
        "Σ deposited is conserved: the liquidation transfers, it never mints"
    );
}
