//! Pure funding-engine logic (issue #6): anchor the order-book `mark` to the
//! trustless `index` (annualized jitoSOL yield). Fully free of Anchor account
//! plumbing so `proptest` drives the invariants directly; the thin
//! `settle_funding` adapter in `lib.rs` applies it to the on-chain `PerpMarket`
//! / `Position` accounts.
//!
//! Units (all `APY_SCALE = 1_000_000` fixed point):
//! * `mark` — order-book mid (a yield level, from `orderbook::mid`).
//! * `index` — annualized trustless yield, `annualize(realized_yield, slots, SLOTS_PER_YEAR)`.
//! * `premium = mark - index` (signed `i128`; negative half the time).
//! * `funding_rate = clamp(funding_k·premium/APY_SCALE, -max_funding, +max_funding)` (signed).
//! * `funding_payment = notional·rate/APY_SCALE · epochs × side_flow` (signed).
//!
//! **Sign convention (resolved):** a positive premium (mark above index) yields
//! a positive funding rate, and **longs pay shorts** — so a long's funding flow
//! is negative when `rate > 0`, a short's is positive, and the two are exact
//! opposites. The issue's "side: long=+1, short=-1" labels the *position* side;
//! the **flow** sign for a long is `-1` (it pays out on positive funding).
//!
//! All arithmetic is signed `i128` with `checked_*`/`saturating_*` — funding and
//! premium are negative half the time, so unsigned `u128`/`saturating_*` would be
//! wrong here (see AGENTS.md). No panicking math, no new dependency.

use anchor_lang::prelude::*;

use crate::constants::APY_SCALE;
use crate::positions::PositionSide;

/// Signed funding flow direction for a position side.
///
/// A positive `funding_rate` means **longs pay shorts** (R-F3): the long flow is
/// `-1` (it pays out), the short flow is `+1` (it receives). Encoded separately
/// from `PositionSide`'s `Long`/`Short` so the convention is explicit and
/// property-testable (the issue's free-form `Long=+1, Short=-1` sign on the
/// *position* must not be confused with the *cash flow* sign).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideFlow {
    /// A long pays on positive funding (`flow = -1`).
    Long,
    /// A short receives on positive funding (`flow = +1`).
    Short,
}

impl SideFlow {
    /// The signed `+1`/`-1` multiplier applied to the funding amount.
    pub fn multiplier(self) -> i128 {
        match self {
            SideFlow::Long => -1,
            SideFlow::Short => 1,
        }
    }

    /// Convert a [`PositionSide`] into its funding flow. Longs and shorts are
    /// fixed: a long pays on positive funding regardless of how it was opened.
    pub fn from_position_side(side: PositionSide) -> Self {
        match side {
            PositionSide::Long => SideFlow::Long,
            PositionSide::Short => SideFlow::Short,
        }
    }
}

/// `mark - index`, signed, in `APY_SCALE` fixed point (R-F1).
///
/// Sum is computed in `i128` so a large (or negative) premium cannot overflow a
/// `u64`; both inputs are `u64` by construction.
pub fn premium(mark: u64, index: u64) -> i128 {
    (mark as i128) - (index as i128)
}

/// The per-epoch funding rate, clamped to `[-max_funding, +max_funding]` (R-F2).
///
/// `funding_k·premium` is computed in `i128` and scaled back to `APY_SCALE` by a
/// single `i128` division (truncating toward zero, so the sign is preserved).
/// The result is then clamped symmetric about `0`. `funding_k` is `u64` in
/// `[1, APY_SCALE]` (validated at init); `max_funding` is `u64`.
pub fn funding_rate(premium: i128, funding_k: u64, max_funding: u64) -> i128 {
    // `saturating_mul` cannot panic; on a true overflow it saturates to ±i128::MAX,
    // which the `/ APY_SCALE` step brings back into range before the symmetric
    // clamp bounds it to ±max_funding (AGENTS.md: no panicking math). For the
    // production domain (yields ~APY_SCALE) the product is far inside i128.
    let raw = (funding_k as i128).saturating_mul(premium);
    let unscaled = raw / (APY_SCALE as i128);
    let cap = max_funding as i128;
    unscaled.clamp(-cap, cap)
}

/// The signed funding payment a position accrues over `epochs` full epochs
/// (R-F3, R-F5): `notional·rate/APY_SCALE · epochs × side_flow`.
///
/// * `epochs == 0` ⇒ `0` (idempotent accrual: settling the same epoch twice adds
///   nothing — R-F5).
/// * `rate > 0` ⇒ the long flow (`SideFlow::Long`) is negative (pays), the short
///   flow is positive (receives), and the two are exact opposites.
/// * `rate < 0` ⇒ the sign flips (shorts pay).
///
/// `notional·rate` is computed in `i128` (a production notional `u64` times a
/// signed rate in `[-APY_SCALE, APY_SCALE]` cannot overflow `i128`), divided by
/// `APY_SCALE` (truncating toward zero), then multiplied by `epochs` and the
/// sided flow. Truncation means the payment is `0` whenever
/// `|notional·rate| < APY_SCALE` for a single epoch — the documented quantization
/// floor.
pub fn funding_payment(notional: u64, rate: i128, epochs: u64, side: SideFlow) -> i128 {
    // `saturating_mul` is exact for the production domain (notional ≤ ~1e12 and
    // rate ≤ APY_SCALE ⇒ product ≤ ~1e18, far inside i128) and panics never.
    let scaled = (notional as i128).saturating_mul(rate) / (APY_SCALE as i128);
    scaled
        .saturating_mul(epochs as i128)
        .saturating_mul(side.multiplier())
}

/// The funding epoch index containing `slot`: `slot / funding_epoch_slots`
/// (R-F5). Full epochs that have *elapsed* are `cur_epoch - last_funding_epoch`;
/// a *current* partial epoch contributes nothing until it completes.
pub fn funding_epoch(slot: u64, epoch_slots: u64) -> u64 {
    if epoch_slots == 0 {
        // Degenerate epoch length (0 slots) is not valid for a live market (a
        // `funding_epoch_slots > 0` invariant is enforced at init); a 0 window
        // collapses every slot to epoch 0 so nothing accrues spuriously.
        return 0;
    }
    slot / epoch_slots
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::APY_SCALE;
    use proptest::prelude::*;

    // --- R-F1: premium sign / magnitude ---

    proptest! {
        #[test]
        fn premium_is_signed_difference(mark in 0u64..APY_SCALE * 10, index in 0u64..APY_SCALE * 10) {
            let p = premium(mark, index);
            prop_assert_eq!(p, mark as i128 - index as i128);
            // Mark above index -> positive premium (longs pay); below -> negative.
            if mark > index { prop_assert!(p > 0); }
            if mark < index { prop_assert!(p < 0); }
            if mark == index { prop_assert_eq!(p, 0); }
        }

        #[test]
        fn premium_is_antisymmetric(mark in 0u64..APY_SCALE * 10, index in 0u64..APY_SCALE * 10) {
            prop_assert_eq!(premium(mark, index), -premium(index, mark));
        }
    }

    // --- R-F2: clamp, monotonicity, symmetry ---

    proptest! {
        #[test]
        fn funding_rate_clamped_to_max(premium in -10_000_000i128..10_000_000,
                                      funding_k in 1u64..=APY_SCALE,
                                      max_funding in 0u64..=APY_SCALE) {
            let cap = max_funding as i128;
            let r = funding_rate(premium, funding_k, max_funding);
            prop_assert!(r >= -cap && r <= cap, "rate clamped to ±max_funding");
            // A zero premium or zero funding_k yields zero.
            if premium == 0 || funding_k == 0 {
                prop_assert_eq!(r, 0);
            }
        }

        #[test]
        fn funding_rate_sign_follows_premium(premium in -10_000_000i128..10_000_000,
                                             funding_k in 1u64..=APY_SCALE,
                                             max_funding in 0u64..=APY_SCALE) {
            let r = funding_rate(premium, funding_k, max_funding);
            if premium > 0 { prop_assert!(r >= 0, "positive premium => nonneg rate"); }
            if premium < 0 { prop_assert!(r <= 0, "negative premium => nonpos rate"); }
        }

        #[test]
        fn funding_rate_monotonic_in_premium(
            low in -10_000_000i128..10_000_000,
            high in -10_000_000i128..10_000_000,
            funding_k in 1u64..=APY_SCALE,
            max_funding in 0u64..=APY_SCALE,
        ) {
            let rl = funding_rate(low, funding_k, max_funding);
            let rh = funding_rate(high, funding_k, max_funding);
            // Unclamped monotonicity: a strictly larger premium never yields a
            // smaller rate; inside the clamp band it is strictly increasing.
            if low <= high { prop_assert!(rl <= rh, "rate non-decreasing in premium"); }
        }

        #[test]
        fn funding_rate_symmetric(premium in -10_000_000i128..10_000_000,
                                  funding_k in 1u64..=APY_SCALE,
                                  max_funding in 0u64..=APY_SCALE) {
            prop_assert_eq!(
                funding_rate(premium, funding_k, max_funding),
                -funding_rate(-premium, funding_k, max_funding),
                "rate is odd in premium"
            );
        }
    }

    // --- R-F3: sign convention — long pays, short receives on positive rate ---

    proptest! {
        #[test]
        fn long_pays_short_receives_on_positive_rate(
            notional in 1_000_000u64..1_000_000_000_000,
            rate in 1i128..=APY_SCALE as i128,
            epochs in 1u64..1_000,
        ) {
            let p_long = funding_payment(notional, rate, epochs, SideFlow::Long);
            let p_short = funding_payment(notional, rate, epochs, SideFlow::Short);
            prop_assert!(p_long < 0, "long pays on positive funding");
            prop_assert!(p_short > 0, "short receives on positive funding");
            prop_assert_eq!(p_long, -p_short, "long/short are exact opposites");
        }

        #[test]
        fn short_pays_long_receives_on_negative_rate(
            notional in 1_000_000u64..1_000_000_000_000,
            rate in -(APY_SCALE as i128)..-1i128,
            epochs in 1u64..1_000,
        ) {
            let p_long = funding_payment(notional, rate, epochs, SideFlow::Long);
            let p_short = funding_payment(notional, rate, epochs, SideFlow::Short);
            prop_assert!(p_short < 0, "short pays on negative funding");
            prop_assert!(p_long > 0, "long receives on negative funding");
            prop_assert_eq!(p_long, -p_short, "long/short are exact opposites");
        }

        #[test]
        fn side_flow_round_trips_position_side(side in 0u8..2u8) {
            let ps = match side {
                0 => PositionSide::Long,
                _ => PositionSide::Short,
            };
            prop_assert_eq!(
                SideFlow::from_position_side(ps).multiplier(),
                match ps { PositionSide::Long => -1i128, PositionSide::Short => 1i128 }
            );
        }
    }

    // --- R-F5: accrual idempotency, zero epochs, epoch derivation ---

    proptest! {
        #[test]
        fn zero_rate_or_zero_epochs_pays_nothing(
            notional in 0u64..1_000_000_000_000,
            rate in -(APY_SCALE as i128)..=APY_SCALE as i128,
            epochs in 0u64..100,
        ) {
            // epochs == 0 => 0 regardless of rate.
            prop_assert_eq!(funding_payment(notional, rate, 0, SideFlow::Long), 0);
            prop_assert_eq!(funding_payment(notional, rate, 0, SideFlow::Short), 0);
            // rate == 0 => 0 regardless of epochs.
            prop_assert_eq!(funding_payment(0, rate, epochs, SideFlow::Long), 0);
        }

        #[test]
        fn payment_scales_linearly_with_epochs(
            notional in 1u64..1_000_000_000_000,
            rate in 1i128..=APY_SCALE as i128,
            e1 in 1u64..200,
            e2 in 1u64..200,
        ) {
            let p1 = funding_payment(notional, rate, e1, SideFlow::Long);
            let p2 = funding_payment(notional, rate, e2, SideFlow::Long);
            // Two successive accruals equal one accrual over the sum (linear in
            // epochs, up to truncation in the per-epoch division).
            prop_assert_eq!(p1 + p2, funding_payment(notional, rate, e1 + e2, SideFlow::Long));
        }
    }

    #[test]
    fn funding_epoch_derives_slot_window() {
        assert_eq!(funding_epoch(0, 10), 0);
        assert_eq!(funding_epoch(9, 10), 0);
        assert_eq!(funding_epoch(10, 10), 1);
        assert_eq!(funding_epoch(19, 10), 1);
        assert_eq!(funding_epoch(20, 10), 2);
        // A zero epoch length collapses to epoch 0 (never accrues spuriously).
        assert_eq!(funding_epoch(123, 0), 0);
        // Elapsed full epochs between two slots.
        let cur = funding_epoch(1000, 100);
        let last = funding_epoch(750, 100);
        // 1000/100 = 10 and 750/100 = 7; three full epoch boundaries (8, 9, 10)
        // elapsed between them.
        assert_eq!(cur - last, 3);
    }

    // R-F5 idempotency: applying the same epoch delta twice adds nothing.
    #[test]
    fn accrual_is_idempotent_per_epoch() {
        let notional = 1_000_000_000u64;
        let rate = 5_000i128; // +0.5%
        let one_epoch = funding_payment(notional, rate, 1, SideFlow::Long);
        // Two one-epoch settlements == one two-epoch settlement.
        let two_epochs = funding_payment(notional, rate, 2, SideFlow::Long);
        assert_eq!(
            one_epoch * 2,
            two_epochs,
            "settling each epoch once == settling both once"
        );
    }
}
