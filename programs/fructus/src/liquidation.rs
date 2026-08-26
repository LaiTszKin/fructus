//! Pure liquidation-engine logic (issue #8): the health predicate and the
//! penalty/partial/full liquidation transitions. Free of Anchor account plumbing
//! so `proptest` drives the invariants directly; the thin `liquidate` adapter in
//! `lib.rs` applies it to the on-chain `Position` / `UserCollateral` accounts.
//!
//! Model (units = APY_SCALE / USDC microunits, all signed where required):
//! * `unrealized_pnl` — **signed** (`i128`) index-based PnL computed from the
//!   position's entry running sums vs the current trustless index
//!   (`positions::pnl`). Health is computed against the order-book TWAP reference
//!   (R-L1) at the on-chain adapter; this module is price-agnostic and takes the
//!   already-marked PnL, so it stays purely property-testable.
//! * `equity = collateral + unrealized_pnl` (signed; a long losing money has a
//!   negative equity contribution).
//! * `maintenance_margin = margin_required(notional, maintenance_bps)` (ceiling,
//!   reuses `positions::margin_required`).
//! * **Liquidatable** iff `equity < maintenance_margin` — a **strict** `<`; an
//!   exactly-maintained position (`equity == maintenance`) is healthy (R-L2). A
//!   position is therefore liquidated only when it is genuinely under-margin.
//! * **Penalty**: a bps fraction of the collateral released by the liquidation,
//!   paid to the liquidator out of the position's collateral (R-L3) — the
//!   liquidator incentive. The surviving collateral is re-derived at the
//!   **initial** margin ratio (`state.rs`: `position.collateral ==
//!   margin_required(notional, initial_margin_bps)`), so the released collateral
//!   is `position_collateral − margin_required(notional − amount,
//!   initial_margin_bps)`; for a full liquidation it is the whole
//!   `position_collateral` (a closed position holds zero collateral).
//!
//! All arithmetic is signed `i128` with `checked_*`/`saturating_*` — no panicking
//! math (AGENTS.md).

use anchor_lang::prelude::*;

use crate::positions::margin_required;

/// Reason a liquidation transition cannot be applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiquidateError {
    /// `amount == 0` or `amount > notional`.
    InvalidAmount,
    /// A `u64`/`i128` intermediate overflowed.
    Overflow,
}

impl From<LiquidateError> for crate::FructusError {
    fn from(e: LiquidateError) -> Self {
        match e {
            LiquidateError::InvalidAmount => crate::FructusError::InvalidSize,
            LiquidateError::Overflow => crate::FructusError::ArithmeticOverflow,
        }
    }
}

/// Signed equity: `collateral + unrealized_pnl`, in `i128` (R-L2). A loss shows up
/// as a negative unrealized contribution, reducing equity.
pub fn equity(collateral: u64, unrealized_pnl: i128) -> i128 {
    (collateral as i128).saturating_add(unrealized_pnl)
}

/// Maintenance margin requirement for `notional` at `maintenance_bps` (ceiling
/// division, `u128` intermediates) — reuses [`crate::positions::margin_required`].
/// `None` only on overflow (total on its domain).
pub fn maintenance_margin(notional: u64, maintenance_bps: u16) -> Option<u64> {
    margin_required(notional, maintenance_bps)
}

/// Whether a position is liquidatable: `equity < maintenance_margin` (strict
/// `<` — equality is healthy; R-L2). A zero-notional position has no exposure and
/// is never liquidatable. `None` only on non-total overflow.
pub fn liquidatable(
    collateral: u64,
    unrealized_pnl: i128,
    notional: u64,
    maintenance_bps: u16,
) -> Option<bool> {
    if notional == 0 {
        return Some(false);
    }
    let maintenance = maintenance_margin(notional, maintenance_bps)?;
    Some(equity(collateral, unrealized_pnl) < (maintenance as i128))
}

/// The liquidator penalty on `collateral` at `penalty_bps`: ceiling division
/// `collateral·penalty_bps/10_000` (R-L3). `None` only on overflow. Guarantees
/// `penalty <= collateral` for `penalty_bps <= 10_000`.
pub fn liquidation_penalty(collateral: u64, penalty_bps: u16) -> Option<u64> {
    let exact = (collateral as u128)
        .checked_mul(penalty_bps as u128)?
        .checked_add(9_999)?;
    u64::try_from(exact / 10_000).ok()
}

/// Apply a liquidation of `amount` notional to a position (R-L3), preserving the
/// documented `Position.collateral == margin_required(notional,
/// initial_margin_bps)` invariant (see `state.rs`) — the SAME invariant that
/// `apply_open_fills` / `apply_close_fills` maintain on the open/close paths.
///
/// * `amount == 0` or `amount > notional` → [`LiquidateError::InvalidAmount`].
/// * The position is backed at the **initial** margin ratio, so liquidating
///   `amount` reduces the surviving exposure to `notional - amount` and the
///   position keeps `remaining = margin_required(notional - amount,
///   initial_margin_bps)` (clamped to the collateral actually held). For a full
///   liquidation (`amount == notional`) the surviving notional is `0` and
///   `margin_required(0, _) == 0` — a closed (zero-notional) position holds
///   **zero** collateral, all of it released.
/// * The collateral freed by the liquidation is `released = position_collateral -
///   remaining`; the liquidator reward is the penalty share of that freed
///   collateral (`liquidation_penalty(released, penalty_bps)`, which never
///   exceeds `released`, so `remaining + reward ≤ position_collateral` — no value
///   is created and the surviving collateral never goes negative).
///
/// Returns the **position's** remaining collateral and the liquidator's reward.
/// The caller derives the remaining notional (`notional − amount`) and credits
/// the reward to the liquidator's collateral ledger. Both return values are
/// always non-negative (never an insolvent negative remaining collateral).
///
/// `maintenance_bps` is retained for signature/API stability; the collateral
/// release is computed at the initial-margin ratio (the documented invariant),
/// not at the maintenance ratio — `maintenance_bps` remains the **health**
/// threshold (`liquidatable`), not a release parameter.
pub fn apply_liquidation(
    position_collateral: u64,
    notional: u64,
    amount: u64,
    initial_margin_bps: u16,
    _maintenance_bps: u16,
    penalty_bps: u16,
) -> std::result::Result<(u64, u64), LiquidateError> {
    if amount == 0 || amount > notional {
        return Err(LiquidateError::InvalidAmount);
    }
    // Surviving exposure-backed collateral at the INITIAL margin ratio — the
    // documented invariant. `surviving_notional <= notional` and
    // `margin_required` is monotonic non-decreasing in notional, so under the
    // invariant `remaining <= position_collateral`. Clamp to the collateral
    // actually held for non-invariant (arbitrary) callers, so the surviving
    // collateral never exceeds what the position held.
    let surviving_notional = notional - amount; // amount <= notional (checked above)
    let remaining = margin_required(surviving_notional, initial_margin_bps)
        .ok_or(LiquidateError::Overflow)?
        .min(position_collateral);
    // Collateral actually freed by the liquidation.
    let released = position_collateral.saturating_sub(remaining);
    // Liquidator reward = penalty share of the freed collateral.
    // `liquidation_penalty` guarantees `reward <= released`, so
    // `remaining + reward <= remaining + released == position_collateral`.
    let reward = liquidation_penalty(released, penalty_bps).ok_or(LiquidateError::Overflow)?;
    Ok((remaining, reward))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::APY_SCALE;
    use proptest::prelude::*;

    // --- R-L2: liquidatable boundary + monotonicity ---

    proptest! {
        #[test]
        fn liquidatable_equality_is_healthy(
            notional in 1u64..1_000_000_000_000,
            maintenance_bps in 1u16..=10_000,
        ) {
            let maintenance = maintenance_margin(notional, maintenance_bps).unwrap();
            // equity == maintenance => NOT liquidatable (strict <).
            prop_assert_eq!(
                liquidatable((maintenance as i128) as u64, 0, notional, maintenance_bps),
                Some(false)
            );
            // equity just below maintenance => liquidatable.
            let below = maintenance.checked_sub(1).unwrap();
            prop_assert_eq!(
                liquidatable(below, 0, notional, maintenance_bps),
                Some(true)
            );
        }

        #[test]
        fn liquidatable_monotonic_in_pnl(
            collateral in 0u64..1_000_000_000_000,
            notional in 1u64..1_000_000_000_000,
            maintenance_bps in 1u16..=10_000,
            pnl_high in -10_000_000i128..10_000_000,
        ) {
            let pnl_low = pnl_high - 1;
            let hi = liquidatable(collateral, pnl_high, notional, maintenance_bps).unwrap();
            let lo = liquidatable(collateral, pnl_low, notional, maintenance_bps).unwrap();
            // A more negative PnL (lower equity) can only make it MORE liquidatable.
            prop_assert!(lo || !hi, "lower pnl never un-liquidatably becomes healthy");
        }

        #[test]
        fn liquidatable_monotonic_in_maintenance(
            collateral in 0u64..1_000_000_000_000,
            notional in 1u64..1_000_000_000_000,
            mm_high in 1u16..=10_000,
            pnl in -10_000_000i128..10_000_000,
        ) {
            let mm_low = if mm_high > 1 { mm_high - 1 } else { 1 };
            let hi = liquidatable(collateral, pnl, notional, mm_high).unwrap();
            let lo = liquidatable(collateral, pnl, notional, mm_low).unwrap();
            // A higher maintenance requirement can only make it MORE liquidatable.
            prop_assert!(hi || !lo, "higher maintenance never un-liquidatably becomes healthy");
        }

        #[test]
        fn zero_notional_or_no_collateral_is_not_liquidatable(
            collateral in 0u64..1_000_000,
            maintenance_bps in 1u16..=10_000,
            pnl in -10_000i128..10_000,
        ) {
            // notional == 0 => maintenance == 0 => equity >= 0 is healthy.
            prop_assert_eq!(liquidatable(collateral, pnl, 0, maintenance_bps), Some(false));
        }
    }

    // --- R-L3: penalty bounds + monotonicity ---

    proptest! {
        #[test]
        fn penalty_is_zero_at_bps_zero(collateral in 0u64..1_000_000_000_000) {
            prop_assert_eq!(liquidation_penalty(collateral, 0), Some(0));
            prop_assert_eq!(liquidation_penalty(0, 10_000), Some(0));
        }

        #[test]
        fn penalty_never_exceeds_collateral(
            collateral in 0u64..1_000_000_000_000,
            penalty_bps in 0u16..=10_000,
        ) {
            let p = liquidation_penalty(collateral, penalty_bps).unwrap();
            prop_assert!(p <= collateral, "penalty bounded by collateral");
        }

        #[test]
        fn penalty_is_full_at_bps_10000(collateral in 0u64..1_000_000_000_000) {
            prop_assert_eq!(liquidation_penalty(collateral, 10_000), Some(collateral));
        }

        #[test]
        fn penalty_monotonic_in_bps(
            collateral in 1u64..1_000_000_000_000,
            bps_low in 0u16..10_000,
        ) {
            let bps_high = bps_low + 1;
            let lo = liquidation_penalty(collateral, bps_low).unwrap();
            let hi = liquidation_penalty(collateral, bps_high).unwrap();
            prop_assert!(hi >= lo, "penalty non-decreasing in bps");
        }
    }

    // --- R-L3: full / partial liquidation transitions ---

    proptest! {
        #[test]
        fn full_liquidation_zeroes_exposure(
            notional in 1u64..1_000_000_000_000,
            initial_margin_bps in 1u16..=10_000,
            maintenance_bps in 1u16..=10_000,
            penalty_bps in 1u16..=10_000,
        ) {
            // Only a valid (maintenance < initial) market is reachable on-chain.
            prop_assume!(maintenance_bps < initial_margin_bps);
            // A real position is backed at the INITIAL margin ratio (state.rs).
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
            // Full liquidation closes the position (notional -> 0): its surviving
            // collateral must be margin_required(0, _) == 0 (all collateral released).
            prop_assert_eq!(remaining, margin_required(0, initial_margin_bps).unwrap());
            prop_assert_eq!(remaining, 0);
            let released = position_collateral - remaining;
            prop_assert_eq!(reward, liquidation_penalty(released, penalty_bps).unwrap());
            prop_assert!(remaining >= 0, "remaining collateral never negative");
            prop_assert!(reward >= 0, "reward never negative");
            prop_assert!(remaining + reward <= position_collateral, "no value created");
        }

        #[test]
        fn partial_liquidation_consumes_only_the_backed_portion(
            notional in 2u64..1_000_000_000_000,
            amount in 1u64..=1_000_000_000_000u64,
            initial_margin_bps in 1u16..=10_000,
            maintenance_bps in 1u16..=10_000,
            penalty_bps in 1u16..=10_000,
        ) {
            prop_assume!(maintenance_bps < initial_margin_bps);
            let amount = if amount > notional { notional } else { amount };
            let position_collateral = margin_required(notional, initial_margin_bps).unwrap();
            let (remaining, reward) = apply_liquidation(
                position_collateral,
                notional,
                amount,
                initial_margin_bps,
                maintenance_bps,
                penalty_bps,
            )
            .unwrap();
            // The surviving collateral equals margin_required(notional - amount,
            // initial_margin_bps) — the documented invariant (like apply_close_fills).
            prop_assert_eq!(
                remaining,
                margin_required(notional - amount, initial_margin_bps).unwrap()
            );
            let released = position_collateral - remaining;
            prop_assert_eq!(reward, liquidation_penalty(released, penalty_bps).unwrap());
            prop_assert!(remaining >= 0, "remaining collateral never negative");
            prop_assert!(remaining + reward <= position_collateral, "no value created");
        }

        #[test]
        fn invalid_amounts_rejected(
            notional in 1u64..1_000_000_000_000,
            initial_margin_bps in 1u16..=10_000,
            maintenance_bps in 1u16..=10_000,
            penalty_bps in 1u16..=10_000,
        ) {
            let position_collateral = margin_required(notional, initial_margin_bps).unwrap();
            prop_assert_eq!(
                apply_liquidation(position_collateral, notional, 0, initial_margin_bps, maintenance_bps, penalty_bps),
                Err(LiquidateError::InvalidAmount)
            );
            let too_big = notional.saturating_add(1);
            prop_assert_eq!(
                apply_liquidation(position_collateral, notional, too_big, initial_margin_bps, maintenance_bps, penalty_bps),
                Err(LiquidateError::InvalidAmount)
            );
        }
    }

    #[test]
    fn penalty_and_collateral_bounds_pinned() {
        // Ceiling division pins.
        assert_eq!(liquidation_penalty(1_000, 500).unwrap(), 50); // 5% of 1000
        assert_eq!(liquidation_penalty(1, 500).unwrap(), 1); // ceil(0.05) = 1
        assert_eq!(liquidation_penalty(0, 500).unwrap(), 0);
        // A penalty can never exceed the underlying collateral (boundary).
        assert!(liquidation_penalty(u64::MAX, 10_000).unwrap() <= u64::MAX);
    }

    #[test]
    fn liquidation_leaves_no_insolvency() {
        // A full liquidation must never leave a negative remaining collateral,
        // and must not create value out of thin air.
        let notional = 1_000_000u64;
        let initial_bps = 2_000u16; // 20% initial margin
        let maintenance_bps = 1_000u16; // 10% maintenance (health threshold)
        let position_collateral = margin_required(notional, initial_bps).unwrap();
        for penalty_bps in [1u16, 500, 10_000] {
            let (remaining, reward) = apply_liquidation(
                position_collateral,
                notional,
                notional,
                initial_bps,
                maintenance_bps,
                penalty_bps,
            )
            .unwrap();
            assert!(remaining >= 0);
            assert!(reward >= 0);
            // Full liquidation closes the position: surviving collateral == 0.
            assert_eq!(remaining, 0);
            // Holder total (remaining) + liquidator reward <= position collateral.
            assert!(remaining + reward <= position_collateral);
        }
    }

    // ==== Adversarial-review invariants for liquidation.rs (moved from review_tests.rs) ====
    // These independently pin the liquidation contract (liquidatable, penalty,
    // apply_liquidation) across the FULL domain, so the implementation's own
    // in-file tests (which share its assumptions) cannot mask a counterexample.

    // Domain band constant (from the design doc): notional is a USDC amount
    // (microunits) far below u64::MAX.
    const NOTIONAL_MAX: u64 = 1_000_000_000_000; // 1e12 (design band)

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

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        // The `liquidate` handler's zero-sum transition: releases `consumed` from
        // the victim's `reserved`, debits the victim's `deposited` by `reward`,
        // credits the liquidator's `deposited` by the same `reward`. For every
        // reachable (notional, amount, im, mm, penalty) with the position backed
        // at the INITIAL margin ratio, the reward is drawn strictly out of the
        // victim's released margin, so Σ(victim + liquidator) deposited is
        // conserved and the victim's ledger never underflows.
        #[test]
        fn liquidation_transition_conserves_and_never_underflows(
            notional in 1u64..NOTIONAL_MAX,
            amount in 1u64..NOTIONAL_MAX,
            im in 1u16..=10_000,
            mm in 1u16..=10_000,
            penalty in 0u16..=10_000,
            free_balance in 0u64..1_000_000_000_000u64,
            liquidator_deposited in any::<u64>(),
        ) {
            let amount = if amount > notional { notional } else { amount };
            // A real position is backed at the INITIAL margin ratio (state.rs).
            let position_collateral = margin_required(notional, im).unwrap();
            let (remaining, reward) =
                apply_liquidation(position_collateral, notional, amount, im, mm, penalty).unwrap();

            let consumed = position_collateral - remaining; // released collateral
            let reserved_before = position_collateral; // single-position victim ledger

            // A valid ledger: deposited = reserved + free (free >= 0), so reserved
            // never exceeds deposited. Both operands are bounded (<= ~2e12 < u64::MAX).
            let victim_deposited = reserved_before.checked_add(free_balance).unwrap();
            let victim_after = victim_deposited - reward; // handler uses checked_sub
            let reserved_after = reserved_before - consumed;
            // The handler credits the liquidator via `checked_add`; a reward that
            // overflows `u64` reverts the transaction (safe: no value is created),
            // so model exactly the handler's accepted path.
            let Some(liquidator_after) = liquidator_deposited.checked_add(reward) else {
                return Ok(());
            };

            prop_assert!(reward <= consumed, "reward drawn from the victim's released margin");
            prop_assert!(victim_after >= reserved_after, "free seam holds after liquidation");
            // Zero-sum: nothing minted, nothing burned (in u128 so the sum cannot
            // overflow the ledger-position invariant).
            prop_assert_eq!(
                (victim_deposited as u128) + (liquidator_deposited as u128),
                (victim_after as u128) + (liquidator_after as u128),
                "a liquidation is a zero-sum transfer across victim + liquidator"
            );
            // The surviving position keeps exactly margin_required(notional-amount, im).
            prop_assert_eq!(remaining, margin_required(notional - amount, im).unwrap());
        }
    }

    /// Regression (critical): a liquidation must be a ZERO-SUM transfer. The
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
}
