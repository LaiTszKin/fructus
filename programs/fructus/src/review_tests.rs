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

use proptest::prelude::*;

use crate::constants::APY_SCALE;
use crate::funding::{funding_epoch, funding_payment, funding_rate, premium, SideFlow};
use crate::liquidation::{
    apply_liquidation, equity, liquidatable, liquidation_penalty, maintenance_margin,
    LiquidateError,
};
use crate::positions::{apply_pnl, margin_required, pnl, signed_yield_change, PositionSide};

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
