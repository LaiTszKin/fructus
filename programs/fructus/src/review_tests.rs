//! Independent adversarial property-based review tests (issues #6–#11).
//!
//! These are written by the REVIEW AGENT, NOT the implementation. They pin the
//! formal invariants of `funding.rs`, `liquidation.rs`, `positions.rs` and the
//! `settle_funding` / `settle_close` / `liquidate` / `close_position` adapters
//! from a SEPARATE spec (see the formal spec table). They deliberately push
//! against the production domain edges and across full integer ranges to
//! surface counterexamples the implementation's own in-file tests (which share
//! the author's assumptions) may mask.
//!
//! None of these tests may be "relaxed to pass": a property that fails on a
//! legitimate input is a confirmed defect and MUST be reported as-is.

use anchor_lang::prelude::*;
use proptest::prelude::*;

use crate::constants::APY_SCALE;
use crate::funding::{funding_epoch, funding_payment, funding_rate, premium, SideFlow};
use crate::liquidation::{
    apply_liquidation, equity, liquidatable, liquidation_penalty, maintenance_margin,
    LiquidateError,
};
use crate::positions::{apply_pnl, margin_required, pnl, signed_yield_change, PositionSide};

// Position-lifecycle adapters (private crate-root helpers, so this module — a
// child of the crate root — can drive the REAL `apply_open_fills` /
// `apply_close_fills` rather than a reproduction).
use crate::state::{Position, UserCollateral};
use crate::{apply_close_fills, apply_open_fills};

/// Production domain bounds (from the design doc / init validation): notional is
/// a USDC amount (microunits) that floats well below `u64::MAX`; `funding_k`,
/// `max_funding` are validated to `[1, APY_SCALE]` / `[0, APY_SCALE]`.
const NOTIONAL_MAX: u64 = 1_000_000_000_000; // 1e12 (design band)
const RATE_MAX: i128 = APY_SCALE as i128 * 10; // ±10x APY_SCALE (generous)
const EPOCHS_MAX: u64 = 10_000;

// ===========================================================================
// funding.rs — R-F1/R-F2/R-F3/R-F5
// ===========================================================================

proptest! {
    // R-F1: premium is the exact signed difference, antisymmetric, and 0 iff
    // equal — for the FULL u64 domain, not just the `APY_SCALE * 10` band.
    #[test]
    fn premium_is_exact_signed_difference_full_domain(
        mark in any::<u64>(),
        index in any::<u64>(),
    ) {
        let p = premium(mark, index);
        prop_assert_eq!(p, mark as i128 - index as i128,
            "premium must equal mark-index exactly");
        prop_assert_eq!(p, -premium(index, mark), "premium is antisymmetric");
        prop_assert_eq!(p == 0, mark == index, "premium==0 iff mark==index");
        if mark > index { prop_assert!(p > 0, "mark>index => positive premium"); }
        if mark < index { prop_assert!(p < 0, "mark<index => negative premium"); }
    }

    // R-F2: clamp is exact and the rate is symmetric/odd across a WIDE premium
    // range (full i128), so a premium at the extremes still clamps to ±max_funding
    // rather than spilling past it.
    #[test]
    fn funding_rate_never_exceeds_cap_full_premium(
        premium_value in any::<i128>(),
        funding_k in 1u64..=APY_SCALE,
        max_funding in 0u64..=APY_SCALE,
    ) {
        let cap = max_funding as i128;
        let r = funding_rate(premium_value, funding_k, max_funding);
        prop_assert!(r >= -cap && r <= cap, "rate clamped to ±max_funding");
        if premium_value == 0 { prop_assert_eq!(r, 0, "zero premium => zero rate"); }
    }

    #[test]
    fn funding_rate_is_odd_full_premium(
        premium_value in any::<i128>(),
        funding_k in 1u64..=APY_SCALE,
        max_funding in 0u64..=APY_SCALE,
    ) {
        prop_assert_eq!(
            funding_rate(premium_value, funding_k, max_funding),
            -funding_rate(-premium_value, funding_k, max_funding),
            "funding_rate must be odd in its premium argument"
        );
    }

    #[test]
    fn funding_rate_monotonic_in_premium_full_range(
        low in any::<i128>(),
        high in any::<i128>(),
        funding_k in 1u64..=APY_SCALE,
        max_funding in 0u64..=APY_SCALE,
    ) {
        let (lo, hi) = if low <= high { (low, high) } else { (high, low) };
        let rl = funding_rate(lo, funding_k, max_funding);
        let rh = funding_rate(hi, funding_k, max_funding);
        prop_assert!(rl <= rh, "funding_rate is non-decreasing in premium");
    }

    // R-F2: the documented formula `clamp(k*premium/APY_SCALE, ±max_funding)`
    // must hold exactly (independent reference, not the implementation's clamp).
    #[test]
    fn funding_rate_matches_reference_formula(
        premium_value in -RATE_MAX..RATE_MAX,
        funding_k in 1u64..=APY_SCALE,
        max_funding in 0u64..=APY_SCALE,
    ) {
        // Reference: saturating_i128_mul then truncate-divide, then clamp.
        let raw = (funding_k as i128).saturating_mul(premium_value);
        let unscaled = raw / (APY_SCALE as i128);
        let cap = max_funding as i128;
        let expected = if unscaled > cap { cap }
            else if unscaled < -cap { -cap }
            else { unscaled };
        prop_assert_eq!(funding_rate(premium_value, funding_k, max_funding), expected);
    }

    // R-F3: on positive rate, LONG pays / SHORT receives; on negative rate the
    // sign flips; and the two are always exact opposites for identical inputs.
    // This is the core sign convention — asserted over a WIDE notional band.
    #[test]
    fn funding_sign_convention_long_short_exact_opposites(
        notional in 1u64..NOTIONAL_MAX,
        rate in -RATE_MAX..RATE_MAX,
        epochs in 1u64..EPOCHS_MAX,
    ) {
        let p_long = funding_payment(notional, rate, epochs, SideFlow::Long);
        let p_short = funding_payment(notional, rate, epochs, SideFlow::Short);
        prop_assert_eq!(p_long, -p_short, "long and short are exact opposites");

        // Sign convention with the documented quantization floor: the flow is 0
        // (never the wrong sign) whenever |notional*rate| < APY_SCALE.
        if rate > 0 {
            prop_assert!(p_long <= 0, "positive rate => long never gains");
            prop_assert!(p_short >= 0, "positive rate => short never loses");
            // Strictly negative when the scaled amount is nonzero.
            let scaled = (notional as i128).saturating_mul(rate) / (APY_SCALE as i128);
            if scaled > 0 {
                prop_assert!(p_long < 0, "positive funding => long pays (strict)");
                prop_assert!(p_short > 0, "positive funding => short receives (strict)");
            }
        } else if rate < 0 {
            prop_assert!(p_long >= 0, "negative rate => long never loses");
            prop_assert!(p_short <= 0, "negative rate => short never gains");
            let scaled = (notional as i128).saturating_mul(rate) / (APY_SCALE as i128);
            if scaled < 0 {
                prop_assert!(p_long > 0, "negative funding => long receives (strict)");
                prop_assert!(p_short < 0, "negative funding => short pays (strict)");
            }
        }
    }

    // R-F3 formula exactness: `funding_payment = notional*rate/APY_SCALE*epochs*flow`.
    #[test]
    fn funding_payment_matches_reference_formula(
        notional in 0u64..NOTIONAL_MAX,
        rate in -RATE_MAX..RATE_MAX,
        epochs in 0u64..EPOCHS_MAX,
    ) {
        let scaled = (notional as i128).saturating_mul(rate) / (APY_SCALE as i128);
        let expected = scaled.saturating_mul(epochs as i128);
        prop_assert_eq!(funding_payment(notional, rate, epochs, SideFlow::Long), -expected);
        prop_assert_eq!(funding_payment(notional, rate, epochs, SideFlow::Short), expected);
    }

    // R-F5: epochs == 0 is a no-op for ANY rate/notional; rate == 0 is a no-op.
    #[test]
    fn funding_zero_epochs_and_zero_rate_pay_nothing_full_domain(
        notional in any::<u64>(),
        rate in any::<i128>(),
        epochs in any::<u64>(),
    ) {
        prop_assert_eq!(funding_payment(notional, rate, 0, SideFlow::Long), 0,
            "epochs==0 => zero payment");
        prop_assert_eq!(funding_payment(notional, rate, 0, SideFlow::Short), 0);
        prop_assert_eq!(funding_payment(notional, 0, epochs, SideFlow::Long), 0,
            "rate==0 => zero payment");
        prop_assert_eq!(funding_payment(notional, 0, epochs, SideFlow::Short), 0);
    }

    // R-F5: accrual is LINEAR in epochs (idempotent per-epoch): n two-epoch
    // accruals == one (e1+e2) accrual, and re-settling an epoch adds nothing.
    #[test]
    fn funding_epoch_additivity(
        notional in 1u64..NOTIONAL_MAX,
        rate in -RATE_MAX..RATE_MAX,
        e1 in 0u64..EPOCHS_MAX,
        e2 in 0u64..EPOCHS_MAX,
    ) {
        let p1 = funding_payment(notional, rate, e1, SideFlow::Long);
        let p2 = funding_payment(notional, rate, e2, SideFlow::Long);
        let total = funding_payment(notional, rate, e1 + e2, SideFlow::Long);
        // saturating add of the two epoch payments equals the combined payment
        // (additivity of the two-epoch accrual; exact in the production band).
        prop_assert_eq!(p1.saturating_add(p2), total,
            "accrual is additive in epochs");
    }

    // R-F5: epoch derivation is integer floor division, monotonic in slot, and
    // degenerate zero epoch length collapses to epoch 0.
    #[test]
    fn funding_epoch_is_floor_division_monotonic(
        slot in any::<u64>(),
        epoch_slots in 1u64..,
    ) {
        prop_assert_eq!(funding_epoch(slot, epoch_slots), slot / epoch_slots);
        let next = slot.saturating_add(1);
        prop_assert!(funding_epoch(next, epoch_slots) >= funding_epoch(slot, epoch_slots),
            "epoch is monotonic in slot");
        prop_assert_eq!(funding_epoch(0, 0), 0, "zero epoch_slots collapses");
        prop_assert_eq!(funding_epoch(1_000_000, 0), 0, "zero epoch_slots collapses large slot");
    }
}

// ===========================================================================
// liquidation.rs — R-L2/R-L3/R-L4
// ===========================================================================

proptest! {
    // R-L2: strict '<' — equity == maintenance is healthy, just below is
    // liquidatable; for a wide notional/bps band (NOT only the small band the
    // implementation chose).
    #[test]
    fn liquidatable_strict_inequality_boundary(
        notional in 1u64..NOTIONAL_MAX,
        bps in 1u16..=10_000,
    ) {
        let maintenance = maintenance_margin(notional, bps).unwrap();
        // equity == maintenance => NOT liquidatable.
        prop_assert_eq!(liquidatable(maintenance, 0, notional, bps), Some(false),
            "equity==maintenance is healthy (strict <)");
        // equity == maintenance - 1 => liquidatable.
        let below = maintenance - 1;
        prop_assert_eq!(liquidatable(below, 0, notional, bps), Some(true),
            "equity just below maintenance is liquidatable");
        // equity == maintenance + 1 => not liquidatable.
        let above = maintenance + 1;
        prop_assert_eq!(liquidatable(above, 0, notional, bps), Some(false),
            "equity just above maintenance is healthy");
    }

    #[test]
    fn liquidatable_zero_notional_never_liquidatable(
        collateral in any::<u64>(),
        pnl_value in any::<i128>(),
        bps in any::<u16>(),
    ) {
        prop_assert_eq!(liquidatable(collateral, pnl_value, 0, bps), Some(false),
            "zero-notional position is never liquidatable");
    }

    #[test]
    fn liquidatable_health_equals_collateral_plus_pnl(
        collateral in any::<u64>(),
        pnl_value in any::<i128>(),
        notional in 1u64..NOTIONAL_MAX,
        bps in 0u16..=10_000,
    ) {
        let e = equity(collateral, pnl_value);
        let m = maintenance_margin(notional, bps);
        // `liquidatable` MUST be exactly `equity < maintenance` (with the
        // zero-notional short-circuit ahead of it). Compare the branch precisely.
        match m {
            Some(mm) => {
                let expected = (e as i128) < (mm as i128);
                prop_assert_eq!(liquidatable(collateral, pnl_value, notional, bps), Some(expected),
                    "liquidatable must be exactly equity < maintenance");
            }
            None => {
                // maintenance overflow (never on the u64 x u16 domain).
                prop_assert_eq!(liquidatable(collateral, pnl_value, notional, bps), None);
            }
        }
    }

    #[test]
    fn liquidatable_never_panics_or_none_on_total_domain(
        collateral in any::<u64>(),
        pnl_value in any::<i128>(),
        notional in any::<u64>(),
        bps in 0u16..=10_000,
    ) {
        // `maintenance_margin` is total on u64 x u16 (no u128 overflow), so
        // `liquidatable` must return `Some(..)` for EVERY input, never `None`
        // and never panic.
        prop_assert!(liquidatable(collateral, pnl_value, notional, bps).is_some());
    }
}

proptest! {
    // R-L3 penalty exactness: `ceil(collateral*bps/10000)` for a wide band.
    #[test]
    fn liquidation_penalty_exact_ceiling_formula(
        collateral in any::<u64>(),
        penalty_bps in 0u16..=10_000,
    ) {
        let exact = (collateral as u128)
            .checked_mul(penalty_bps as u128)
            .unwrap()
            .checked_add(9_999)
            .unwrap();
        let expected = (exact / 10_000) as u64;
        prop_assert_eq!(liquidation_penalty(collateral, penalty_bps), Some(expected),
            "penalty must be ceil(collateral*bps/10000)");
    }

    #[test]
    fn liquidation_penalty_bounds_and_extremes(
        collateral in any::<u64>(),
        penalty_bps in 0u16..=10_000,
    ) {
        let p = liquidation_penalty(collateral, penalty_bps).unwrap();
        prop_assert!(p <= collateral, "penalty bounded by collateral");
        if penalty_bps == 0 { prop_assert_eq!(p, 0, "0 bps => 0 penalty"); }
        if penalty_bps == 10_000 { prop_assert_eq!(p, collateral, "10_000 bps => full collateral"); }
    }

    #[test]
    fn liquidation_penalty_monotonic_full_collateral(
        collateral in any::<u64>(),
        bps_low in 0u16..10_000,
    ) {
        let bps_high = bps_low + 1;
        let lo = liquidation_penalty(collateral, bps_low).unwrap();
        let hi = liquidation_penalty(collateral, bps_high).unwrap();
        prop_assert!(hi >= lo, "penalty non-decreasing in bps");
    }
}

proptest! {
    // R-L3/R-L4 apply_liquidation: never negative remaining, no value created,
    // invalid amounts rejected, and a FULL liquidation empties the notional.
    #[test]
    fn apply_liquidation_preserves_collateral(
        position_collateral in any::<u64>(),
        notional in 1u64..NOTIONAL_MAX,
        amount in 1u64..NOTIONAL_MAX,
        initial_margin_bps in 1u16..=10_000,
        maintenance_bps in 1u16..=10_000,
        penalty_bps in 0u16..=10_000,
    ) {
        let amount = if amount > notional { notional } else { amount };
        let (remaining, reward) =
            apply_liquidation(position_collateral, notional, amount, initial_margin_bps, maintenance_bps, penalty_bps).unwrap();
        prop_assert!(remaining >= 0, "remaining collateral never negative");
        prop_assert!(reward >= 0, "reward never negative");
        prop_assert!(remaining <= position_collateral, "remaining <= position collateral");
        prop_assert!(reward <= position_collateral, "reward <= position collateral");
        prop_assert!(remaining + reward <= position_collateral,
            "no value created: remaining + reward <= position collateral");
    }

    #[test]
    fn apply_liquidation_invalid_amount_rejected(
        position_collateral in any::<u64>(),
        notional in any::<u64>(),
        initial_margin_bps in any::<u16>(),
        maintenance_bps in any::<u16>(),
        penalty_bps in any::<u16>(),
    ) {
        // amount == 0 => InvalidAmount regardless of everything else.
        prop_assert_eq!(
            apply_liquidation(position_collateral, notional, 0, initial_margin_bps, maintenance_bps, penalty_bps),
            Err(LiquidateError::InvalidAmount)
        );
        // amount > notional => InvalidAmount.
        let too_big = notional.saturating_add(1).max(1);
        prop_assert_eq!(
            apply_liquidation(position_collateral, notional, too_big, initial_margin_bps, maintenance_bps, penalty_bps),
            Err(LiquidateError::InvalidAmount)
        );
    }

    #[test]
    fn apply_liquidation_full_releases_all_collateral(
        notional in 1u64..NOTIONAL_MAX,
        initial_margin_bps in 1u16..=10_000,
        maintenance_bps in 1u16..=10_000,
        penalty_bps in 0u16..=10_000,
    ) {
        // A real position is backed at the INITIAL margin ratio (state.rs
        // invariant). A FULL liquidation (`amount == notional`) closes the
        // position (notional -> 0), so its surviving collateral must be
        // margin_required(0, _) == 0 — all collateral released, never leaving a
        // negative remaining.
        prop_assume!(maintenance_bps < initial_margin_bps);
        let position_collateral = margin_required(notional, initial_margin_bps).unwrap();
        let (remaining, reward) = apply_liquidation(
            position_collateral,
            notional,
            notional,
            initial_margin_bps,
            maintenance_bps,
            penalty_bps,
        )
        .unwrap();
        prop_assert_eq!(remaining, margin_required(0, initial_margin_bps).unwrap());
        prop_assert_eq!(remaining, 0, "full liquidation never leaves negative remaining");
        prop_assert!(reward >= 0, "reward never negative");
        prop_assert!(remaining + reward <= position_collateral, "no value created");
    }
}

// ===========================================================================
// positions.rs — apply_pnl / pnl / margin_required / accumulate_entry
// ===========================================================================

proptest! {
    // R-S3: apply_pnl never returns None on a loss, never returns negative, and
    // clamps at exactly `deposited - |loss|` while a profit credits exactly.
    #[test]
    fn apply_pnl_loss_clamps_at_zero_full_domain(
        deposited in any::<u64>(),
        loss in 1i128..=i128::MAX,
    ) {
        let out = apply_pnl(deposited, -loss).expect("apply_pnl is total on a loss");
        let want = deposited.saturating_sub(loss.min(u64::MAX as i128) as u64);
        prop_assert_eq!(out, want, "loss debits but clamps at the deposited floor");
        prop_assert!(out <= deposited, "a loss never increases deposited");
        prop_assert!(out >= 0, "a loss never makes deposited negative");
    }

    #[test]
    fn apply_pnl_profit_credits_or_none_on_overflow(
        deposited in any::<u64>(),
        gain in 0i128..=i128::MAX,
    ) {
        let out = apply_pnl(deposited, gain);
        let want = (deposited as i128) + gain;
        if want <= u64::MAX as i128 {
            prop_assert_eq!(out, Some(want as u64), "profit credits exactly");
        } else {
            prop_assert_eq!(out, None, "profit past u64::MAX is None (checked), never a wrap");
        }
    }

    #[test]
    fn apply_pnl_zero_is_identity(deposited in any::<u64>()) {
        prop_assert_eq!(apply_pnl(deposited, 0), Some(deposited));
    }
}

proptest! {
    // pnl: exact antisymmetry (long == -short), sign correctness, and the
    // quantization floor — across a WIDE notional/rate band.
    #[test]
    fn pnl_long_short_exact_antisymmetry(
        n in 100_000_000_000_000u64..100_000_000_000_000_000u64,
        d in 100_000_000_000_000u64..100_000_000_000_000_000u64,
        cur_n in 100_000_000_000_000u64..100_000_000_000_000_000u64,
        cur_d in 100_000_000_000_000u64..100_000_000_000_000_000u64,
        w in 1_000_000u64..1_000_000_000_000u64,
        notional in 1u64..1_000_000_000_000_000_000u64,
    ) {
        let (ns, ds) = crate::positions::accumulate_entry(0, 0, n, d, w)
            .expect("accumulate_entry should not overflow in band");
        let p_long = pnl(ns, ds, cur_n, cur_d, notional, PositionSide::Long)
            .expect("pnl in band");
        let p_short = pnl(ns, ds, cur_n, cur_d, notional, PositionSide::Short)
            .expect("pnl in band");
        // Both Some in the production band; assert exact antisymmetry + sign.
        prop_assert_eq!(p_long, -p_short, "long and short pnl are exact opposites");
        let change = signed_yield_change(ns, ds, cur_n, cur_d).expect("change in band");
        if change > 0 {
            prop_assert!(p_long >= 0, "index up => long non-negative");
        } else if change < 0 {
            prop_assert!(p_long <= 0, "index down => long non-positive");
        } else {
            prop_assert_eq!(p_long, 0, "no change => zero pnl");
        }
    }

    #[test]
    fn margin_required_ceiling_and_monotonic_full_domain(
        notional in any::<u64>(),
        bps in 1u16..=10_000,
    ) {
        let m = margin_required(notional, bps).expect("margin_required is total on the validated bps domain");
        let exact = (notional as u128) * (bps as u128);
        let expected = ((exact + 9_999) / 10_000) as u64;
        prop_assert_eq!(m, expected, "margin_required must be ceil(notional*bps/10000)");
        // Monotonic in notional.
        let m_next = margin_required(notional + 1, bps).unwrap();
        prop_assert!(m_next >= m, "margin_required is monotonic in notional");
        if bps == 10_000 { prop_assert_eq!(m, notional, "10_000 bps => 1x"); }
    }
}

// ===========================================================================
// adapter-level (invariants the lib.rs handlers promise) — pure-expressible
// ===========================================================================

proptest! {
    // settle_funding / settle_close: `apply_pnl` applied to the signed payment
    // / realized PnL preserves the "always a valid deposited amount" invariant
    // for EVERY valid (deposited, signed_amount) pair: positive credits,
    // negative clamps at 0, never an invalid/nil written amount.
    #[test]
    fn settle_transition_deposited_never_invalid(
        deposited in any::<u64>(),
        signed_amount in any::<i128>(),
    ) {
        let out = apply_pnl(deposited, signed_amount);
        if let Some(v) = out {
            prop_assert!(v <= u64::MAX, "deposited stays a valid u64");
            if signed_amount < 0 {
                prop_assert!(v <= deposited, "a debit never increases deposited");
            }
        }
    }

    // R-F5 idempotency across the adapter: re-settling the same epoch delta
    // (points already accrued) adds nothing.
    #[test]
    fn settlement_idempotent_same_epoch(
        notional in 1u64..NOTIONAL_MAX,
        rate in -RATE_MAX..RATE_MAX,
        epochs in 0u64..EPOCHS_MAX,
    ) {
        // First settle accrues the epoch delta; a second settle on the same
        // (now-updated) last_funding_epoch yields epochs == 0 => 0, so the
        // accumulator advances by exactly the first payment.
        let first = funding_payment(notional, rate, epochs, SideFlow::Long);
        let second = funding_payment(notional, rate, 0, SideFlow::Long);
        prop_assert_eq!(second, 0, "re-settling an accrued epoch adds nothing");
        let accumulated = first.saturating_add(second);
        prop_assert_eq!(accumulated, first, "idempotent accrual is net-additive");
    }
}

// ===========================================================================
// Adapter-level: position re-open must preserve the accounting basis of the
// PRIOR life. `apply_open_fills` resets `entry_n_sum` / `entry_d_sum` /
// `open_slot` on a re-open but leaves `closed_notional` and
// `last_funding_epoch` stale, so a generation that was fully closed and then
// re-opened (before `settle_close`) gets its prior `closed_notional` settled at
// the NEW life's entry rate, and its prior closed funding epochs re-charged
// against the new notional.
// ===========================================================================

/// Build a zeroed, inert `Position` (no side assumptions in the pure adapters).
fn life_pos() -> Position {
    Position {
        market: Pubkey::default(),
        owner: Pubkey::default(),
        side: 0,
        notional: 0,
        entry_n_sum: 0,
        entry_d_sum: 0,
        collateral: 0,
        last_funding_epoch: 0,
        closed_notional: 0,
        closed_entry_n_sum: 0,
        closed_entry_d_sum: 0,
        open_slot: 0,
        bump: 0,
    }
}

fn life_uc(deposited: u64) -> UserCollateral {
    UserCollateral {
        deposited,
        reserved: 0,
        bump: 0,
    }
}

fn life_fill(size: u64) -> crate::orderbook::Fill {
    crate::orderbook::Fill {
        maker_seq: 0,
        maker_owner: Pubkey::from([2u8; 32]),
        taker_owner: Pubkey::from([1u8; 32]),
        size,
        price: 10,
    }
}

proptest! {
    // R-S1/R-S2 invariant: the PnL of the closed notional must be determinable
    // from the entry basis that was in effect when it was closed. A re-open
    // (which resets the entry sums) must NOT reframe it. Here the SAME
    // `closed_notional` (generation-1 amount, closed at entry rate r1) is
    // settled both before and after a generation-2 re-open at a DIFFERENT rate
    // r2; the two must agree.
    #[test]
    fn reopen_does_not_reframe_closed_pnl_basis(
        r1 in 1_000_000u64..10_000_000u64,
        r2 in 1_000_000u64..10_000_000u64,
        cur in 1_000_000u64..20_000_000u64,
        amt1 in 1_000_000u64..100_000_000u64,
        amt2 in 1_000_000u64..100_000_000u64,
        im in 2u16..=10_000u16,
    ) {
        prop_assume!(r1 != r2, "need two distinct generations");
        let mut position = life_pos();
        let mut uc = life_uc(1_000_000_000_000_000u64);

        // Generation 1: open at rate r1, then fully close.
        apply_open_fills(&mut position, &mut uc, im, &[life_fill(amt1)], r1, 1, 1, 100).unwrap();
        apply_close_fills(&mut position, &mut uc, im, &[life_fill(amt1)]).unwrap();
        prop_assert_eq!(position.notional, 0, "generation 1 fully closed");
        let gen1_closed = position.closed_notional;
        let (gen1_n, gen1_d) = (position.entry_n_sum, position.entry_d_sum);

        // Reference: settle the generation-1 closed notional at its own entry basis.
        let pnl_at_gen1_basis =
            pnl(gen1_n, gen1_d, cur, 1, gen1_closed, PositionSide::Long);

        // Generation 2 re-open: resets the entry sums to rate r2.
        apply_open_fills(&mut position, &mut uc, im, &[life_fill(amt2)], r2, 1, 2, 100).unwrap();
        prop_assert_eq!(position.closed_notional, gen1_closed, "close storage unchanged by re-open");

        // The handler settles the SAME gen-1 closed notional at the entry basis
        // recorded when it was closed (`closed_entry_*`), which a re-open must
        // leave intact — so the re-open cannot reframe it.
        let pnl_at_gen2_basis = pnl(
            position.closed_entry_n_sum,
            position.closed_entry_d_sum,
            cur,
            1,
            position.closed_notional,
            PositionSide::Long,
        );

        if let (Some(expected), Some(actual)) = (pnl_at_gen1_basis, pnl_at_gen2_basis) {
            prop_assert_eq!(
                expected,
                actual,
                "a re-open must not reframe the prior closed_notional's PnL (it was settled at the new entry rate)"
            );
        }
    }

    // R-F5 invariant: a position re-open must re-base `last_funding_epoch` so the
    // reopened notional only accrues funding over epochs it actually held
    // notional, never over the interval it was closed (notional == 0). Here the
    // position's `last_funding_epoch` is stale (from before the closed period)
    // and is NOT advanced by `apply_open_fills` on re-open.
    #[test]
    fn reopen_does_not_rebase_funding_epoch(
        stale_last_epoch in 0u64..5_000u64,
        reopen_slot in 5_000_000u64..20_000_000u64,
        epoch_slots in 100u64..1_000u64,
        im in 2u16..=10_000u16,
    ) {
        let reopen_epoch = funding_epoch(reopen_slot, epoch_slots);
        prop_assume!(reopen_epoch > stale_last_epoch, "the closed interval is non-empty");

        let mut position = life_pos();
        let mut uc = life_uc(1_000_000_000_000_000u64);
        position.last_funding_epoch = stale_last_epoch;

        // Generation 1 was closed (notional == 0) and then re-opened.
        let amt = 1_000_000u64;
        apply_open_fills(&mut position, &mut uc, im, &[life_fill(amt)], 2_000_000, 1, reopen_slot, epoch_slots)
            .unwrap();

        // The handler's settle_funding charges `cur_epoch - position.last_funding_epoch`
        // epochs against the CURRENT notional. The correct basis after a re-open
        // is the re-open epoch, so those epoch deltas must be re-based there.
        prop_assert_eq!(
            position.last_funding_epoch,
            reopen_epoch,
            "a re-open must re-base last_funding_epoch to the re-open epoch; \
             otherwise the reopened notional pays funding for the closed interval"
        );
    }
}

/// Deterministic minimal witness for the re-open PnL-basis bug.
#[test]
fn reopen_reframes_closed_pnl_witness() {
    // Generation 1: enter long at rate 2.0 (n=2,d=1,w=1e6), close fully.
    // Generation 2: re-open at rate 3.0. Index currently 4.0.
    // The gen-1 closed 1000 notional's TRUE PnL (entry 2.0 -> 4.0) is +1000;
    // the handler settles it at the gen-2 entry 3.0 -> 4.0, giving ~333.
    let mut position = life_pos();
    let mut uc = life_uc(1_000_000_000_000_000u64);
    let im = 1_000u16;
    let amt = 1_000u64;

    apply_open_fills(&mut position, &mut uc, im, &[life_fill(amt)], 2, 1, 1, 100).unwrap();
    apply_close_fills(&mut position, &mut uc, im, &[life_fill(amt)]).unwrap();
    let gen1_closed = position.closed_notional;
    assert_eq!(gen1_closed, amt, "fully closed generation 1");
    assert_eq!(
        position.entry_n_sum,
        (2u128) * (amt as u128),
        "gen1 entry rate 2.0"
    );
    assert_eq!(position.entry_d_sum, (1u128) * (amt as u128));

    let reference = pnl(
        position.entry_n_sum,
        position.entry_d_sum,
        4,
        1,
        gen1_closed,
        PositionSide::Long,
    )
    .expect("pnl in band");
    assert_eq!(
        reference, amt as i128,
        "gen1 closed at entry 2.0 -> 4.0 is +{amt}"
    );

    // Re-open generation 2 at rate 3.0 (fresh entry sums, closed_notional stale).
    apply_open_fills(&mut position, &mut uc, im, &[life_fill(amt)], 3, 1, 2, 100).unwrap();
    assert_eq!(
        position.closed_notional, gen1_closed,
        "closed_notional NOT reset on re-open"
    );
    // The handler settles at the NEW entry rate: wrong result.
    let wrong = pnl(
        position.entry_n_sum,
        position.entry_d_sum,
        4,
        1,
        position.closed_notional,
        PositionSide::Long,
    )
    .expect("pnl in band");
    assert_ne!(
        wrong, reference,
        "re-open reframed the gen-1 closed {}, settling it at the gen-2 entry rate \
         ({wrong}) instead of the gen-1 entry rate ({reference})",
        gen1_closed
    );
}

// ===========================================================================
// Deterministic unit regression tests (plain fixed inputs, no proptest): these
// pin the three confirmed counterexamples from the adversarial review on the
// EXACT minimal inputs the review shrank each bug to. They are the narrow,
// fast, always-run regression guard that stays green once the fix lands — vs.
// the proptests above, which generate thousands of inputs.
// ===========================================================================

/// Regression A (critical): a liquidation must be a ZERO-SUM transfer. The
/// handler credits `liquidator_collateral.deposited += reward` and debits
/// `user_collateral.deposited -= reward`; that debit is payable only because
/// `apply_liquidation` guarantees `reward <= released == position_collateral -
/// remaining` (the reward is drawn out of the victim's released margin, so
/// Σ deposited across victim + liquidator is conserved; the vault is never
/// over-issued). Pin the EXACT minimal counterexample the review shrank:
/// `(notional=2, amount=2 [full], im=2, mm=1, penalty_bps=500)`.
#[test]
fn liquidation_is_zero_sum_deterministic() {
    // position_collateral = margin_required(2, initial_margin_bps=2) = ceil(4/1e4) = 1
    let position_collateral = margin_required(2, 2).unwrap();
    assert_eq!(position_collateral, 1);
    // Full liquidation: remaining = margin_required(0, 2) = 0; released = 1;
    // reward = ceil(1 * 500 / 1e4) = 1.
    let (remaining, reward) = apply_liquidation(position_collateral, 2, 2, 2, 1, 500).unwrap();
    assert_eq!((remaining, reward), (0, 1), "shrank minimal counterexample");
    // The zero-sum guarantee the handler relies on (the operands are `u64`, so
    // non-negativity is type-guaranteed):
    assert!(
        remaining + reward <= position_collateral,
        "no value created: remaining + reward <= position_collateral"
    );
    let released = position_collateral.saturating_sub(remaining);
    assert!(
        reward <= released,
        "reward payable out of the victim's released margin (so victim.deposited -= reward is safe)"
    );
    // Model the handler's ledger transition; Σ deposited must be conserved.
    let (victim_before, liquidator_before) = (10_000u64, 0u64);
    let (victim_after, liquidator_after) = (
        victim_before.checked_sub(reward).unwrap(),
        liquidator_before.checked_add(reward).unwrap(),
    );
    assert_eq!(
        victim_after + liquidator_after,
        victim_before + liquidator_before,
        "liquidation mints nothing: Σ deposited is conserved"
    );
    // Deterministic sweep — fixed seeds, all must conserve.
    for (notional, amount, im, mm) in [(3u64, 2u64, 1_000u16, 500u16), (7, 5, 2_000, 1_000)] {
        let pc = margin_required(notional, im).unwrap();
        let (rem, rew) = apply_liquidation(pc, notional, amount, im, mm, 500).unwrap();
        assert!(rem + rew <= pc, "no value created");
        assert!(
            rew <= pc.saturating_sub(rem),
            "reward payable from released margin"
        );
    }
}

/// Regression B2 (critical): a re-open must NOT reframe the entry basis of the
/// prior `closed_notional`. `apply_close_fills` records the close-time entry
/// basis into `closed_entry_*`; a re-open resets the LIVE `entry_*` (fresh
/// basis for the new generation) but leaves `closed_entry_*` intact, so
/// `settle_close` prices the closed amount at its own (close-time) rate.
#[test]
fn reopen_preserves_closed_entry_basis_deterministic() {
    let mut position = life_pos();
    let mut uc = life_uc(1_000_000_000_000_000u64);
    let im = 1_000u16; // 10% initial margin
    let amt = 1_000u64;

    // Generation 1: open long at entry rate 2.0 (n=2, d=1), then fully close.
    apply_open_fills(&mut position, &mut uc, im, &[life_fill(amt)], 2, 1, 1, 100).unwrap();
    apply_close_fills(&mut position, &mut uc, im, &[life_fill(amt)]).unwrap();
    assert_eq!(position.notional, 0, "generation 1 fully closed");
    assert_eq!(position.closed_notional, amt, "closed notional recorded");
    assert_eq!(
        position.closed_entry_n_sum,
        2 * (amt as u128),
        "gen-1 entry basis numerator"
    );
    assert_eq!(
        position.closed_entry_d_sum, amt as u128,
        "gen-1 entry basis denominator"
    );

    // Reference: settle the gen-1 closed notional at its own (close-time) basis.
    let reference = pnl(
        position.closed_entry_n_sum,
        position.closed_entry_d_sum,
        4,
        1,
        position.closed_notional,
        PositionSide::Long,
    )
    .expect("pnl in band");
    assert_eq!(reference, amt as i128, "(4/2 − 1) × {amt} == +{amt}");

    // Generation 2: re-open at a DIFFERENT rate 3.0 — live entry sums reset.
    apply_open_fills(&mut position, &mut uc, im, &[life_fill(amt)], 3, 1, 2, 100).unwrap();
    assert_eq!(
        position.entry_n_sum,
        3 * (amt as u128),
        "live entry basis reset to gen-2"
    );
    assert_eq!(
        position.entry_d_sum, amt as u128,
        "live entry basis reset to gen-2"
    );
    assert_eq!(
        position.closed_entry_n_sum,
        2 * (amt as u128),
        "re-open must NOT reframe closed basis"
    );
    assert_eq!(
        position.closed_entry_d_sum, amt as u128,
        "re-open must NOT reframe closed basis"
    );
    assert_eq!(
        position.closed_notional, amt,
        "closed notional unchanged by re-open"
    );

    // The handler settles at `closed_entry_*` (the close-time basis): correct.
    let actual = pnl(
        position.closed_entry_n_sum,
        position.closed_entry_d_sum,
        4,
        1,
        position.closed_notional,
        PositionSide::Long,
    )
    .expect("pnl in band");
    assert_eq!(
        actual, reference,
        "closed PnL prices at the close-time basis, never the new basis"
    );
    // The buggy (pre-fix) behaviour priced it at the LIVE (gen-2) entry: wrong.
    let buggy = pnl(
        position.entry_n_sum,
        position.entry_d_sum,
        4,
        1,
        position.closed_notional,
        PositionSide::Long,
    )
    .expect("pnl in band");
    assert_ne!(
        buggy, reference,
        "(4/3 − 1) × {amt} != (4/2 − 1) × {amt}: a re-open reframed it"
    );
}

/// Regression B1 (medium): a re-open must re-base `last_funding_epoch` to the
/// re-open epoch so the reopened notional only accrues funding over epochs it
/// actually held notional (never the closed interval, where notional == 0).
#[test]
fn reopen_rebases_funding_epoch_deterministic() {
    let mut position = life_pos();
    let mut uc = life_uc(1_000_000_000_000_000u64);
    let im = 2u16;
    // A position whose `last_funding_epoch` is stale from before the closed
    // interval; it is then re-opened at slot 5,000,000 with epoch length 100.
    position.last_funding_epoch = 0; // stale
    let reopen_slot = 5_000_000u64;
    let epoch_slots = 100u64;
    let amt = 1_000_000u64;
    apply_open_fills(
        &mut position,
        &mut uc,
        im,
        &[life_fill(amt)],
        2_000_000,
        1,
        reopen_slot,
        epoch_slots,
    )
    .unwrap();
    assert_eq!(
        position.last_funding_epoch,
        funding_epoch(reopen_slot, epoch_slots),
        "re-open must re-base last_funding_epoch to the re-open epoch"
    );
    assert_eq!(funding_epoch(reopen_slot, epoch_slots), 50_000);
    assert_ne!(
        position.last_funding_epoch, 0,
        "otherwise the reopened notional pays funding for the closed interval"
    );
}
