//! Independent adversarial review of the liquidation **ledger** transition
//! (updated for [fix A] — the Design A PnL pool).
//!
//! `apply_liquidation` (pure) returns the surviving position collateral and the
//! liquidator reward, and it is correctly bounded (`remaining + reward <=
//! position_collateral`). The `liquidate` handler in `lib.rs` applies that to
//! the `UserCollateral` ledgers:
//!
//! ```text
//! consumed  = position.collateral - remaining_collateral      // released
//! victim.user_collateral.reserved  -= consumed                // margin released
//! reserved_after = victim.user_collateral.reserved            // (after release)
//!
//! // [fix A] book the victim's realized loss into the PnL pool, capped at
//! // deposited - reserved_after - reward (reward payable first):
//! booked    = apply_liquidation_loss(deposited, reserved_after, loss, reward)
//! market.pnl_pool                      += booked              // loss collected
//! victim.user_collateral.deposited     -= booked              // loss realized
//!
//! // the reward stays a ZERO-SUM transfer out of the victim's released margin:
//! victim.user_collateral.deposited      -= reward             // penalty out
//! liquidator_collateral.deposited       += reward             // penalty in
//! ```
//!
//! The reward is paid out of the victim's released margin (`consumed >= reward`),
//! so the victim's `deposited` falls by `reward` and the liquidator's rises by
//! the same amount — that part alone is a zero-sum transfer. With [fix A] the
//! victim's realized loss is **also** booked into the pool, so the FULL
//! transition conserves Σ(deposited + pool): a liquidation transfers value, it
//! never mints it.

use fructus::liquidation::apply_liquidation;
use fructus::positions::margin_required;
use fructus::settlement::apply_liquidation_loss;
use proptest::prelude::*;

const PENALTY_BPS: u16 = 500; // LIQUIDATION_PENALTY_BPS

/// Faithful reproduction of the `liquidate` handler's FULL ledger transition
/// [fix A]: the reward is a transfer OUT of the victim's released margin, and
/// the victim's realized loss is booked into the PnL pool.
///
/// Returns `(victim_deposited_after, liquidator_deposited_after, pool_after,
/// reward, booked)`.
fn handler_transition(
    victim_deposited: u64,
    liquidator_deposited: u64,
    position_collateral: u64,
    notional: u64,
    amount: u64,
    initial_bps: u16,
    maintenance_bps: u16,
    loss: u64, // magnitude of the (negative) index-based realized PnL
) -> Option<(u64, u64, u64, u64, u64)> {
    let (remaining_collateral, reward) = apply_liquidation(
        position_collateral,
        notional,
        amount,
        initial_bps,
        maintenance_bps,
        PENALTY_BPS,
    )
    .ok()?;
    let consumed = position_collateral.saturating_sub(remaining_collateral);
    let reserved_after = position_collateral.saturating_sub(consumed); // reserved - consumed
                                                                       // [fix A] book the loss into the pool, capped at deposited - reserved_after - reward.
    let (victim_after_loss, booked) =
        apply_liquidation_loss(victim_deposited, reserved_after, loss, reward)?;
    let pool_after = booked; // market.pnl_pool += booked (starting pool == 0)
                             // The reward transfer is zero-sum across victim + liquidator.
    let victim_after = victim_after_loss.checked_sub(reward)?;
    let liquidator_after = liquidator_deposited.checked_add(reward)?;
    Some((victim_after, liquidator_after, pool_after, reward, booked))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    // [fix A] The FULL liquidation ledger conserves Σ(deposited + pool): the
    // reward is a zero-sum transfer out of the victim's released margin AND the
    // victim's realized loss is booked into the pool, so
    // Σ(victim.deposited + liquidator.deposited + pool) is unchanged for any
    // liquidation — reward 0 or not, loss 0 or not.
    #[test]
    fn liquidation_conserves_total_deposited_plus_pool(
        (notional, amount, initial_bps, maintenance_bps) in
            (2u64..1_000_000_000u64, 1u64..1_000_000_000u64, 2u16..=10_000u16)
                .prop_flat_map(|(n, a, ib)| (proptest::prelude::Just(n), proptest::prelude::Just(a), proptest::prelude::Just(ib), 1u16..ib)),
        loss in 0u64..1_000_000_000_000u64,
    ) {
        let amount = amount.min(notional);
        // An on-chain position is backed at the INITIAL margin ratio (state.rs).
        let position_collateral = margin_required(notional, initial_bps).expect("total on the validated domain");
        let victim_deposited = position_collateral.saturating_add(100_000_000);
        let liquidator_deposited = 500_000_000u64;

        let (victim_after, liquidator_after, pool_after, _reward, _booked) = handler_transition(
            victim_deposited, liquidator_deposited, position_collateral,
            notional, amount, initial_bps, maintenance_bps, loss,
        ).expect("valid partial/full liquidation");

        let before = (victim_deposited as u128) + (liquidator_deposited as u128);
        let after = (victim_after as u128) + (liquidator_after as u128) + (pool_after as u128);
        prop_assert_eq!(
            after, before,
            "liquidation must not mint collateral: the reward is a zero-sum transfer and the loss is booked into the pool, so Σ(deposited + pool) is conserved"
        );
        // The surviving collateral still backs the surviving exposure, and the
        // victim's free seam is never breached by the loss booking + reward.
        let reserved_after = margin_required(notional - amount, initial_bps).unwrap();
        prop_assert!(
            victim_after >= reserved_after,
            "victim's remaining deposited never breaches the surviving reserved backing"
        );
    }
}

/// Deterministic minimal witness ([fix A]).
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

    // A liquidatable victim: equity (collateral + pnl) is below maintenance.
    // With collateral == 10 and maintenance == margin_required(100, 500) == 5, a
    // loss of 6 makes equity == 4 < 5. The booked loss is capped by the seam.
    let loss = 6u64;
    let (victim_deposited, liquidator_deposited) = (10_000u64, 0u64);
    let (victim_after, liquidator_after, pool_after, reward, booked) = handler_transition(
        victim_deposited,
        liquidator_deposited,
        position_collateral,
        notional,
        amount,
        initial_bps,
        maintenance_bps,
        loss,
    )
    .expect("valid partial liquidation");

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
        victim_deposited - booked - reward,
        "victim deposited debited by the booked loss AND the reward"
    );
    assert_eq!(
        pool_after, booked,
        "the booked loss is collected into the pool"
    );
    assert_eq!(
        (victim_after as u128) + (liquidator_after as u128) + (pool_after as u128),
        (victim_deposited as u128) + (liquidator_deposited as u128),
        "Σ(deposited + pool) is conserved: the liquidation transfers, it never mints"
    );
}
