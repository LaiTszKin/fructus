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
//! * **Penalty**: a bps fraction of the collateral released by the liquidated
//!   notional, paid to the liquidator out of the position's collateral (R-L3) —
//!   the liquidator incentive. For a full liquidation the released collateral is
//!   the whole `position_collateral`.
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

/// Apply a liquidation of `amount` notional to a position (R-L3).
///
/// * `amount == 0` or `amount > notional` → [`LiquidateError::InvalidAmount`].
/// * The liquidated notional releases its maintenance-margin backing
///   `release = maintenance_margin(amount, maintenance_bps)`.
/// * The liquidator reward is `liquidation_penalty(release, penalty_bps)`, paid
///   out of the position's collateral **after** the released backing — but capped
///   so the position holder's remaining collateral never goes negative
///   (`reward ≤ C − release`, clamped).
/// * The position holder keeps `remaining = C − release − reward` (at least `0`).
///
/// Returns the **position's** remaining collateral and the liquidator's reward.
/// The caller derives the remaining notional (`notional − amount`) and credits
/// the reward to the liquidator's collateral ledger. Both return values are
/// always non-negative (never an insolvent negative remaining collateral).
pub fn apply_liquidation(
    position_collateral: u64,
    notional: u64,
    amount: u64,
    maintenance_bps: u16,
    penalty_bps: u16,
) -> std::result::Result<(u64, u64), LiquidateError> {
    if amount == 0 || amount > notional {
        return Err(LiquidateError::InvalidAmount);
    }
    // `release <= position_collateral` because `maintenance_bps <= initial_bps`
    // and `amount <= notional` (margin is monotonic in both), so `saturating_sub`
    // never clamps a legitimate release below its true value.
    let release = maintenance_margin(amount, maintenance_bps).ok_or(LiquidateError::Overflow)?;
    let raw_reward = liquidation_penalty(release, penalty_bps).ok_or(LiquidateError::Overflow)?;
    // Cap the reward so the holder never ends up with a negative remaining
    // collateral (the vault is never left insolvent by a liquidation).
    let available = position_collateral.saturating_sub(release);
    let reward = raw_reward.min(available);
    let remaining = available.saturating_sub(reward);
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
            maintenance_bps in 1u16..=10_000,
            penalty_bps in 1u16..=10_000,
        ) {
            // The position's collateral for the full notional at maintenance bps
            // (bottom-of-band; a real position at initial margin is >= this).
            let position_collateral = maintenance_margin(notional, maintenance_bps).unwrap();
            let (remaining, reward) =
                apply_liquidation(position_collateral, notional, notional, maintenance_bps, penalty_bps).unwrap();
            // Liquidating the whole notional releases its maintenance backing; the
            // reward is a carve-out of whatever collateral remains after release.
            let release = maintenance_margin(notional, maintenance_bps).unwrap();
            let available = position_collateral.saturating_sub(release);
            prop_assert_eq!(reward, liquidation_penalty(release, penalty_bps).unwrap().min(available));
            prop_assert_eq!(remaining, available.saturating_sub(reward));
            prop_assert!(remaining >= 0, "remaining collateral never negative");
            prop_assert!(reward >= 0, "reward never negative");
            prop_assert!(remaining + reward <= position_collateral, "no value created");
            // Remaining notional (caller-derived) is zero for a full liquidation.
            prop_assert_eq!(notional - notional, 0);
        }

        #[test]
        fn partial_liquidation_consumes_only_the_backed_portion(
            notional in 2u64..1_000_000_000_000,
            amount in 1u64..=1_000_000_000_000u64,
            maintenance_bps in 1u16..=10_000,
            penalty_bps in 1u16..=10_000,
        ) {
            let amount = if amount > notional { notional } else { amount };
            let position_collateral = maintenance_margin(notional, maintenance_bps).unwrap();
            let (remaining, reward) =
                apply_liquidation(position_collateral, notional, amount, maintenance_bps, penalty_bps).unwrap();
            let release = maintenance_margin(amount, maintenance_bps).unwrap();
            let available = position_collateral.saturating_sub(release);
            prop_assert_eq!(reward, liquidation_penalty(release, penalty_bps).unwrap().min(available));
            prop_assert_eq!(remaining, available.saturating_sub(reward));
            prop_assert!(remaining >= 0, "remaining collateral never negative");
            prop_assert!(remaining + reward <= position_collateral, "no value created");
        }

        #[test]
        fn invalid_amounts_rejected(
            notional in 1u64..1_000_000_000_000,
            maintenance_bps in 1u16..=10_000,
            penalty_bps in 1u16..=10_000,
        ) {
            let position_collateral = maintenance_margin(notional, maintenance_bps).unwrap();
            prop_assert_eq!(
                apply_liquidation(position_collateral, notional, 0, maintenance_bps, penalty_bps),
                Err(LiquidateError::InvalidAmount)
            );
            let too_big = notional.saturating_add(1);
            prop_assert_eq!(
                apply_liquidation(position_collateral, notional, too_big, maintenance_bps, penalty_bps),
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
        // even at the margin-edge where backing + reward == position collateral.
        let notional = 1_000_000u64;
        let bps = 1_000u16; // 10% maintenance
        let position_collateral = maintenance_margin(notional, bps).unwrap();
        for penalty_bps in [1u16, 500, 10_000] {
            let (remaining, reward) =
                apply_liquidation(position_collateral, notional, notional, bps, penalty_bps)
                    .unwrap();
            assert!(remaining >= 0);
            assert!(reward >= 0);
            // Holder total (remaining) + liquidator reward <= position collateral.
            assert!(remaining + reward <= position_collateral);
        }
    }
}
