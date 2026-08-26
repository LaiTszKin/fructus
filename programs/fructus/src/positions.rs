//! Pure position-lifecycle logic: margin, entry running sums, signed PnL.
//!
//! This module is deliberately free of Anchor account plumbing (mirroring the
//! `orderbook.rs` / `collateral.rs` split): every function is a pure
//! `u64`/`u128`/`i128` transition that `proptest` drives directly, and the
//! thin `open_position` / `close_position` / `settle_fill` adapters in
//! `lib.rs` apply them to the on-chain `Position` / `UserCollateral` accounts.
//!
//! Representation (design doc §5, issue #5):
//! * A position's `notional` is the sum of the notional of every fill that
//!   opened/added to it (USDC microunits, reusing #3's order `size` unit).
//! * The entry index is stored as **notional-weighted running sums**
//!   (`entry_n_sum` / `entry_d_sum`, `u128`) — exact average-cost accounting
//!   with no intermediate rounding. The snapshot rate is
//!   `entry_n_sum / entry_d_sum`, computed at PnL time after a shared
//!   power-of-two normalization (`normalize_sums`), because the raw sums are
//!   O(pool size × cumulative notional) and their direct cross-products
//!   overflow `u128` for production LST pools.
//! * `margin_required` uses **ceiling** division so the reserved collateral is
//!   never below the exact requirement and implied leverage stays at or below
//!   the `initial_margin_bps` cap.
//! * PnL is signed (`i128`, USDC microunits); settlement into `UserCollateral`
//!   is issue #7 — this module only computes it.
//!
//! All arithmetic is `checked_*` / `saturating_*` — no panicking math, no new
//! dependency.

use anchor_lang::prelude::*;

use crate::constants::APY_SCALE;
use crate::error::FructusError;

/// Which side of the market a position holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionSide {
    /// Long — opened by bids; profits when the index rises.
    Long,
    /// Short — opened by asks; profits when the index falls.
    Short,
}

// ---------------------------------------------------------------------------
// Implementation surface (filled in by the implementation task T1).
//
// The signatures below are the contract from design.md §5; the property tests
// at the bottom of this file are the red suite that locks them.
// ---------------------------------------------------------------------------

/// Reserved collateral for `notional` under `initial_margin_bps`:
/// `(notional × bps + 9_999) / 10_000` — CEILING division (u128 intermediates)
/// so `collateral ≥ 1` for `notional ≥ 1` and implied leverage
/// `notional / collateral ∈ [1, 10_000 / bps]`. `None` only on `u128`
/// overflow (unreachable for `u64 × u16` inputs — the function is total on
/// its domain).
pub fn margin_required(notional: u64, initial_margin_bps: u16) -> Option<u64> {
    // CEILING division via `+ 9_999` before `/ 10_000`, in u128 so the
    // `u64 × u16` product can never overflow (the `checked_*` calls keep the
    // "None only on overflow" contract and are unreachable on this domain).
    let exact = (notional as u128)
        .checked_mul(initial_margin_bps as u128)?
        .checked_add(9_999)?;
    u64::try_from(exact / 10_000).ok()
}

/// Accumulate the entry running sums: `cur_n_sum += add_n × add_w`,
/// `cur_d_sum += add_d × add_w` (u128, checked; `None` on overflow). The
/// sums carry the weights, so no current notional argument is needed; the
/// closed→reset case is handled by the adapter (sums := the new fill's
/// weighted snapshot directly), not via a sentinel here.
pub fn accumulate_entry(
    cur_n_sum: u128,
    cur_d_sum: u128,
    add_n: u64,
    add_d: u64,
    add_w: u64,
) -> Option<(u128, u128)> {
    let add_n = (add_n as u128).checked_mul(add_w as u128)?;
    let add_d = (add_d as u128).checked_mul(add_w as u128)?;
    Some((cur_n_sum.checked_add(add_n)?, cur_d_sum.checked_add(add_d)?))
}

/// Normalize the entry running sums into a `u64` rate pair by a SHARED
/// power-of-two shift: `k = max(0, bitlen(max(n_sum, d_sum)) − 45)`. The
/// LARGER sum lands in `[2^44, 2^45)`; the smaller may fall below `2^44`
/// (down to `0` for a degenerate entry). Exact (`k = 0`) when both sums are
/// `< 2^45`. The ratio's relative error is bounded by `2^-44 × (1 + max/min)`
/// — at most ~`2^-34` for the production rate-ratio band `[1, 1e3]`, far
/// below `APY_SCALE`'s 1e-6 granularity.
pub fn normalize_sums(entry_n_sum: u128, entry_d_sum: u128) -> (u64, u64) {
    // Shared power-of-two shift: `k = max(0, bitlen(max) − 45)`. The LARGER
    // sum then lands in `[2^44, 2^45)` and fits `u64`; the smaller may fall
    // below the window (down to `0` for a degenerate entry). Exact (`k = 0`)
    // whenever both sums are `< 2^45`, including the all-zero degenerate pair.
    let shift = (u128::BITS - entry_n_sum.max(entry_d_sum).leading_zeros()).saturating_sub(45);
    ((entry_n_sum >> shift) as u64, (entry_d_sum >> shift) as u64)
}

/// `(rate_current / rate_entry − 1) × APY_SCALE`, WITH sign, computed as
/// `((cur_n × d_e) − (n_e × cur_d)) × APY_SCALE / (n_e × cur_d)` where
/// `(n_e, d_e) = normalize_sums(entry_n_sum, entry_d_sum)` — the same
/// u64-pair cross-multiplied form as `exchange::realized_yield`,
/// overflow-free at production magnitudes (see `normalize_sums`). Sign comes
/// from the numerator. `None` on degenerate inputs (zero entry
/// numerator/denominator) or out-of-band magnitudes far above any real pool.
pub fn signed_yield_change(
    entry_n_sum: u128,
    entry_d_sum: u128,
    cur_n: u64,
    cur_d: u64,
) -> Option<i128> {
    let (n_e, d_e) = normalize_sums(entry_n_sum, entry_d_sum);
    // Degenerate inputs: a zero entry numerator/denominator (the normalized
    // sums carry the entry rate) or a zero current component leaves the
    // ratio undefined.
    if n_e == 0 || d_e == 0 || cur_n == 0 || cur_d == 0 {
        return None;
    }
    // ((cur_n × d_e) − (n_e × cur_d)) × APY_SCALE / (n_e × cur_d).
    // Each product is < 2^64 × 2^45 = 2^109, so the difference fits i128;
    // multiplying by APY_SCALE then overflows i128 only for out-of-band
    // magnitudes far above any real pool — the documented `None` path.
    let cur_x = (cur_n as u128).checked_mul(d_e as u128)?;
    let entry_x = (n_e as u128).checked_mul(cur_d as u128)?;
    let num = cur_x as i128 - entry_x as i128;
    let scaled = num.checked_mul(APY_SCALE as i128)?;
    // `entry_x > 0` here (both factors nonzero); i128 division truncates
    // toward zero, so the sign comes from the numerator.
    Some(scaled / entry_x as i128)
}

/// PnL in signed USDC microunits:
/// `notional × signed_yield_change / APY_SCALE × (+1 Long, −1 Short)`,
/// truncating toward zero (so `pnl == 0` whenever
/// `notional × |signed_yield_change| < APY_SCALE` — the documented
/// quantization floor).
pub fn pnl(
    entry_n_sum: u128,
    entry_d_sum: u128,
    cur_n: u64,
    cur_d: u64,
    notional: u64,
    side: PositionSide,
) -> Option<i128> {
    let change = signed_yield_change(entry_n_sum, entry_d_sum, cur_n, cur_d)?;
    // notional × change / APY_SCALE, truncating toward zero — so the result
    // is 0 exactly when |notional × change| < APY_SCALE (the quantization
    // floor). `checked_mul` is the only real overflow risk; `checked_div` is
    // total (APY_SCALE > 0) but keeps the no-panicking-math discipline, and
    // `checked_neg` guards the Short flip.
    let scaled = (notional as i128)
        .checked_mul(change)?
        .checked_div(APY_SCALE as i128)?;
    match side {
        PositionSide::Long => Some(scaled),
        PositionSide::Short => scaled.checked_neg(),
    }
}

/// Validate `open_position` arguments (extracted so unit tests drive it
/// directly and the handler calls it): `side` must be `0` (Long/Bid) or `1`
/// (Short/Ask) — anything else fails with `ProgramError::InvalidInstructionData`
/// (the existing `side_from_u8` behavior; no `InvalidSide` variant exists) —
/// and `size` must be `> 0` (`InvalidSize`).
pub fn validate_open_args(side: u8, size: u64) -> Result<()> {
    // Mirrors `side_from_u8`: only the SIDE_BID/SIDE_ASK encodings (0/1) are
    // accepted; anything else is a malformed instruction (no InvalidSide
    // variant exists).
    match side {
        crate::SIDE_BID | crate::SIDE_ASK => {}
        _ => return Err(ProgramError::InvalidInstructionData.into()),
    }
    require!(size > 0, FructusError::InvalidSize);
    Ok(())
}

impl PositionSide {
    /// Map the on-chain `position.side` byte (`0` = Long/Bid, `1` = Short/Ask)
    /// back to the [`PositionSide`] enum, or `None` for an invalid encoding.
    ///
    /// The funding/settlement/liquidation handlers read `Position.side` (a `u8`,
    /// matching the book-side encoding) and need the typed side to drive
    /// [`pnl`] / [`crate::funding::SideFlow::from_position_side`]. Mirrors
    /// `crate::side_from_u8`: only the SIDE_BID/SIDE_ASK encodings are accepted.
    pub fn from_side_u8(side: u8) -> Option<Self> {
        match side {
            crate::SIDE_BID => Some(PositionSide::Long),
            crate::SIDE_ASK => Some(PositionSide::Short),
            _ => None,
        }
    }
}

/// Apply signed PnL to the deposited collateral (R-S2, R-S3).
///
/// * `pnl == 0` ⇒ `Some(deposited)` — settled but unchanged.
/// * `pnl > 0` ⇒ `Some(deposited + pnl)` — a profit credits the vault ledger;
///   returns `None` only on a positive overflow (checked add then `u64`
///   conversion, so a gain that exceeds `u64` cannot silently wrap).
/// * `pnl < 0` ⇒ `Some(deposited − |pnl|)` with the loss **clamped at `0`** so
///   `deposited` never goes negative — the vault is never left insolvent by a
///   settlement (R-S3). Clamping is total: a loss never returns `None`.
///
/// This is the pure ledger transition for `settle_close` (and the funding
/// credit/debit via `settle_funding`): it never panics and never over-draws the
/// collateral.
pub fn apply_pnl(deposited: u64, pnl: i128) -> Option<u64> {
    if pnl >= 0 {
        let want = (deposited as u128).checked_add(pnl as u128)?;
        u64::try_from(want).ok()
    } else {
        let debit = pnl.unsigned_abs().min(deposited as u128) as u64;
        Some(deposited - debit)
    }
}

/// Accumulate a newly-closed amount `q` (at per-unit entry-basis components
/// `add_n`/`add_d`) into the closed-entry running sums (R-S2, R-S1).
///
/// `settle_close` prices the pending `closed_notional` in a single
/// `pnl(closed_entry_*, ..., closed_notional)` call, so the closed-entry running
/// sums must be a representation in which each closed generation is realized at
/// **its own** entry basis — not an average that a re-open could reframe. This is
/// the **notional-weighted harmonic mean** of the per-generations' entry rates:
///
/// ```text
/// closed_entry_n_sum / closed_entry_d_sum == closed_notional / Σ (closed_i / rate_i)
/// ```
///
/// i.e. `pnl(closed_entry_*, cur, closed_notional) == Σ_i pnl(closed_i's basis)`.
/// A re-open resets the LIVE `entry_*` but never touches this pair, so a prior
/// closed amount's basis is never reframed (R-S1/R-S2).
///
/// `cur_n_sum` / `cur_d_sum` are the current closed-entry running sums (with the
/// invariant `cur_n_sum == cur_notional * S_den`, `S_den` being the running
/// product of per-generation rate numerators), `cur_notional` the current
/// `closed_notional`, `q` the amount being closed now, and `add_n`/`add_d` the
/// per-unit entry-basis (numerator/denominator) of the amount being closed (the
/// position's avg-cost basis at close time). `None` only on `u128` overflow.
/// Because the scale of `add_n`/`add_d` cancels in the rate ratio, and a
/// single-generation close is scaled by `q` back to the position's entry sums,
/// the closed-entry sums stay consistent with the `Σ` entry-basis convention.
pub fn accumulate_closed_entry(
    cur_n_sum: u128,
    cur_d_sum: u128,
    cur_notional: u64,
    add_n: u64,
    add_d: u64,
    q: u64,
) -> Option<(u128, u128)> {
    // S = Σ (closed_i / rate_i) as a fraction S_num / S_den, with the invariant
    // cur_n_sum == cur_notional * S_den, so S_den = cur_n_sum / cur_notional.
    let (s_num, s_den) = if cur_notional == 0 {
        (0u128, 1u128)
    } else {
        (cur_d_sum, cur_n_sum / cur_notional as u128)
    };
    // S' = S + q * (add_d / add_n) = (s_num*add_n + q*add_d*s_den) / (s_den*add_n).
    let s_den_new = s_den.checked_mul(add_n as u128)?;
    let s_num_new = s_num
        .checked_mul(add_n as u128)?
        .checked_add((q as u128).checked_mul(add_d as u128)?.checked_mul(s_den)?)?;
    let new_notional = (cur_notional as u128).checked_add(q as u128)?;
    let n_new = new_notional.checked_mul(s_den_new)?;
    Some((n_new, s_num_new))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collateral::free_collateral;
    use crate::constants::APY_SCALE;
    use proptest::prelude::*;

    // --- REQ-7: margin / leverage bounds ---

    proptest! {
        #[test]
        fn margin_required_bounds(
            notional in 0u64..1_000_000_000_000,
            bps in 1u16..=10_000,
        ) {
            let m = margin_required(notional, bps)
                .expect("margin_required is total on u64 x u16");
            // ceiling formula
            let exact = (notional as u128) * (bps as u128);
            let expected = ((exact + 9_999) / 10_000) as u64;
            prop_assert_eq!(m, expected, "ceiling formula");
            // bps == 10_000 => exact 1x
            if bps == 10_000 {
                prop_assert_eq!(m, notional, "1x at bps == 10_000");
            }
            // collateral >= 1 for any nonzero notional
            if notional >= 1 {
                prop_assert!(m >= 1, "collateral floor");
            }
            // I-margin-bounds monotonicity item (design.md §5 / PBT plan row):
            // margin_required is monotonic non-decreasing in notional — a
            // larger notional can never require less collateral, so the
            // incremental reservation deltas in apply_open_fills are
            // well-defined (never negative).
            let m_next = margin_required(notional + 1, bps)
                .expect("margin_required is total on u64 x u16");
            prop_assert!(
                m_next >= m,
                "monotonic non-decreasing in notional"
            );
        }

        #[test]
        fn margin_leverage_bound(
            notional in 1u64..1_000_000_000_000,
            bps in 1u16..=10_000,
        ) {
            let m = margin_required(notional, bps).expect("total");
            prop_assert!(m >= 1, "margin > 0 for notional >= 1");
            // leverage = notional / margin <= 10_000 / bps
            // <=> notional * bps <= margin * 10_000 (cross-multiplied, u128)
            prop_assert!(
                (notional as u128) * (bps as u128) <= (m as u128) * 10_000,
                "implied leverage must not exceed the initial-margin cap"
            );
        }

        // --- REQ-8: signed PnL sign and quantization floor ---

        // Entry rate = n0/d0; current above entry: (n0, d0/r) with r in [2, 1000].
        // change >= (r - 1) * APY_SCALE >= APY_SCALE, so the quantization floor
        // (notional * |change| >= APY_SCALE) always holds for notional >= 1.
        #[test]
        fn pnl_sign_long_short(
            n0 in 100_000_000_000_000u64..100_000_000_000_000_000,
            d0 in 100_000_000_000_000u64..100_000_000_000_000_000,
            r in 2u64..=1_000,
            w in 1_000_000u64..1_000_000_000_000,
            notional in 1_000_000u64..1_000_000_000_000_000_000,
        ) {
            let d1 = d0 / r; // current rate = n0/d1 = r * (n0/d0) > entry
            let (n_sum, d_sum) = accumulate_entry(0, 0, n0, d0, w).expect("sum");
            let p_long = pnl(n_sum, d_sum, n0, d1, notional, PositionSide::Long)
                .expect("pnl in band");
            let p_short = pnl(n_sum, d_sum, n0, d1, notional, PositionSide::Short)
                .expect("pnl in band");
            prop_assert!(p_long > 0, "long profits when the index rises");
            prop_assert!(p_short < 0, "short loses when the index rises");
            prop_assert_eq!(p_long, -p_short, "signs are exact opposites");

            // current below entry: (n0/r, d0)
            let n1 = n0 / r;
            let p_long2 = pnl(n_sum, d_sum, n1, d0, notional, PositionSide::Long)
                .expect("pnl in band");
            let p_short2 = pnl(n_sum, d_sum, n1, d0, notional, PositionSide::Short)
                .expect("pnl in band");
            prop_assert!(p_long2 < 0, "long loses when the index falls");
            prop_assert!(p_short2 > 0, "short profits when the index falls");
        }

        #[test]
        fn pnl_zero_when_equal(
            n0 in 100_000_000_000_000u64..100_000_000_000_000_000,
            d0 in 100_000_000_000_000u64..100_000_000_000_000_000,
            w in 1_000_000u64..1_000_000_000_000,
            notional in 1_000_000u64..1_000_000_000_000_000_000,
        ) {
            let (n_sum, d_sum) = accumulate_entry(0, 0, n0, d0, w).expect("sum");
            prop_assert_eq!(
                pnl(n_sum, d_sum, n0, d0, notional, PositionSide::Long),
                Some(0),
                "entry == current => pnl == 0 (long)"
            );
            prop_assert_eq!(
                pnl(n_sum, d_sum, n0, d0, notional, PositionSide::Short),
                Some(0),
                "entry == current => pnl == 0 (short)"
            );
        }

        // REQ-8: no overflow at production magnitudes — every input in the
        // band [1e14, 1e17] with realistic weights/notionals must yield Some.
        #[test]
        fn pnl_no_overflow_at_production_magnitudes(
            n0 in 100_000_000_000_000u64..100_000_000_000_000_000,
            d0 in 100_000_000_000_000u64..100_000_000_000_000_000,
            n1 in 100_000_000_000_000u64..100_000_000_000_000_000,
            d1 in 100_000_000_000_000u64..100_000_000_000_000_000,
            w in 1_000_000u64..1_000_000_000_000,
            notional in 1_000_000u64..1_000_000_000_000_000_000,
        ) {
            let (n_sum, d_sum) = accumulate_entry(0, 0, n0, d0, w).expect("sum");
            prop_assert!(
                signed_yield_change(n_sum, d_sum, n1, d1).is_some(),
                "signed_yield_change must not overflow in band"
            );
            prop_assert!(
                pnl(n_sum, d_sum, n1, d1, notional, PositionSide::Long).is_some(),
                "pnl must not overflow in band"
            );
        }

        // --- REQ-2: entry running sums ---

        // The averaged rate (sn/sd) lies within [min, max] component rate.
        // Components are generated with rate ratio >= 501/500 so the <= 2^-34
        // normalization error cannot flip the comparison.
        #[test]
        fn entry_bounds_from_sums(
            d1 in 10_000_000_000u64..1_000_000_000_000_000,
            d2 in 10_000_000_000u64..1_000_000_000_000_000,
            r1 in 1u64..=500,
            r2 in 501u64..=1_000,
            w1 in 1u64..1_000_000,
            w2 in 1u64..1_000_000,
        ) {
            let n1 = d1 * r1; // rate r1
            let n2 = d2 * r2; // rate r2 > r1
            let (s1n, s1d) = accumulate_entry(0, 0, n1, d1, w1).expect("sum");
            let (sn, sd) = accumulate_entry(s1n, s1d, n2, d2, w2).expect("sum");
            let (an, ad) = normalize_sums(sn, sd);
            // min: rate >= r1  <=>  an >= r1 * ad
            prop_assert!(
                (an as u128) >= (r1 as u128) * (ad as u128),
                "average rate below the min component rate"
            );
            // max: rate <= r2  <=>  an <= r2 * ad
            prop_assert!(
                (an as u128) <= (r2 as u128) * (ad as u128),
                "average rate above the max component rate"
            );
        }

        #[test]
        fn accumulate_entry_matches_component_sums(
            n1 in 1u64..1_000_000_000,
            d1 in 1u64..1_000_000_000,
            n2 in 1u64..1_000_000_000,
            d2 in 1u64..1_000_000_000,
            w1 in 0u64..1_000_000_000,
            w2 in 0u64..1_000_000_000,
        ) {
            // order (1, 2)
            let (a, b) = accumulate_entry(0, 0, n1, d1, w1).expect("sum");
            let (s1n, s1d) = accumulate_entry(a, b, n2, d2, w2).expect("sum");
            // order (2, 1)
            let (c, d) = accumulate_entry(0, 0, n2, d2, w2).expect("sum");
            let (s2n, s2d) = accumulate_entry(c, d, n1, d1, w1).expect("sum");
            prop_assert_eq!((s1n, s1d), (s2n, s2d), "accumulation is order-independent");
            // closed form
            let exp_n = (n1 as u128) * (w1 as u128) + (n2 as u128) * (w2 as u128);
            let exp_d = (d1 as u128) * (w1 as u128) + (d2 as u128) * (w2 as u128);
            prop_assert_eq!((s1n, s1d), (exp_n, exp_d), "closed-form sums");
            // zero weight contributes nothing
            let (z1, z2) = accumulate_entry(0, 0, n1, d1, 0).expect("sum");
            prop_assert_eq!((z1, z2), (0, 0), "zero weight is a no-op");
        }

        // --- REQ-2 / REQ-4: close never drives notional negative, and
        // collateral tracks margin_required(notional) exactly ---

        #[test]
        fn close_never_negative(
            ops in proptest::collection::vec(any::<(bool, u64)>(), 0..40),
            bps in 1u16..=10_000,
        ) {
            let mut notional: u64 = 0;
            for (is_open, size) in ops {
                let size = size % 1_000_000 + 1; // >= 1
                if is_open {
                    notional = notional.checked_add(size).expect("model notional");
                } else if notional >= size {
                    notional -= size;
                }
            }
            prop_assert!(notional >= 0, "notional never negative");
            let m = margin_required(notional, bps).expect("total");
            let exact = ((notional as u128) * (bps as u128) + 9_999) / 10_000;
            prop_assert_eq!(m as u128, exact, "collateral == margin_required(notional)");
            // a full close leaves zero margin
            prop_assert_eq!(margin_required(0, bps), Some(0), "closed => collateral 0");
        }

        // --- REQ-7: reserved = sum of position collateral; free seam ---

        #[test]
        fn reserved_sum_consistency(
            deposited in 0u64..1_000_000_000_000_000,
            long_n in 0u64..100_000_000,
            short_n in 0u64..100_000_000,
            bps in 1u16..=10_000,
        ) {
            let long_c = margin_required(long_n, bps).expect("total");
            let short_c = margin_required(short_n, bps).expect("total");
            let reserved = long_c.checked_add(short_c).expect("model reserved");
            let free = free_collateral(deposited, reserved);
            prop_assert_eq!(
                free,
                deposited.checked_sub(reserved),
                "free == deposited - reserved (None iff reserved > deposited)"
            );
        }
    }

    // --- REQ-7 (A-16): pinned margin formula values ---

    #[test]
    fn margin_required_formula_pinned() {
        for n in [1u64, 10, 1_000, 1_000_000, u64::MAX] {
            // bps == 1000 => ceil(n / 10)
            let expected_1000 = ((n as u128) * 1_000 + 9_999) / 10_000;
            assert_eq!(
                margin_required(n, 1_000).unwrap() as u128,
                expected_1000,
                "n = {n}, bps = 1000"
            );
            // bps == 10_000 => exact 1x
            assert_eq!(
                margin_required(n, 10_000).unwrap(),
                n,
                "n = {n}, bps = 10_000"
            );
        }
        assert_eq!(margin_required(0, 1).unwrap(), 0);
    }

    // --- FR-2 (A-3): notional-zero lifecycle invariant ---
    //
    // The acceptance-documented name for the pure-expressible lifecycle
    // invariants (acceptance.md A-3, filter:
    // `cargo test --workspace position_lifecycle_notional_zero_is_closed`):
    // a closed position has notional == 0, and a closed => zero state implies
    // no reserved collateral (`margin_required(0, _) == Some(0)`), while a
    // re-open on that state resets the entry running sums to the new fill's
    // weighted snapshot (`accumulate_entry(0, 0, n, d, w) == (n·w, d·w)`).

    #[test]
    fn position_lifecycle_notional_zero_is_closed() {
        // Closed => collateral 0 for every valid margin basis-points value
        // (the leverage cap range (0, 10_000] plus the degenerate 1).
        for bps in [1u16, 500, 1_000, 5_000, 10_000] {
            assert_eq!(
                margin_required(0, bps),
                Some(0),
                "closed position (notional == 0) reserves no collateral at {bps} bps"
            );
        }

        // Re-open on the closed (0, 0) state: the entry running sums equal the
        // new fill's weighted snapshot exactly — (n·w, d·w) — for a spread of
        // weights and rates (the re-open reset; A-3/A-9b).
        for (n, d, w) in [
            (1u64, 1u64, 1u64),
            (1_000_000, 1_000_000, 1_000_000),
            (100_000_000_000_000, 1_000_000_000_000, 123_456_789),
            (u64::MAX / 1_000, u64::MAX / 1_000, 1_000),
        ] {
            assert_eq!(
                accumulate_entry(0, 0, n, d, w),
                Some(((n as u128) * (w as u128), (d as u128) * (w as u128))),
                "re-open on a closed position resets entry sums to the weighted snapshot ({n}, {d}) x {w}"
            );
        }
    }

    // --- REQ-3 (A-6): validate_open_args ---

    #[test]
    fn open_position_rejects_zero_size_and_invalid_side() {
        // size == 0 -> error (InvalidSize)
        assert!(validate_open_args(0, 0).is_err());
        assert!(validate_open_args(1, 0).is_err());
        // out-of-range side byte -> error (ProgramError::InvalidInstructionData)
        assert!(validate_open_args(2, 1).is_err());
        assert!(validate_open_args(255, 1).is_err());
        // valid
        assert!(validate_open_args(0, 1).is_ok());
        assert!(validate_open_args(1, 1).is_ok());
    }

    // --- R-F5/R-S2/R-L2: `PositionSide::from_side_u8` (the handlers read the
    // on-chain `position.side` byte and need the typed side to drive
    // `funding::SideFlow` / `positions::pnl`) ---
    //
    // Mirrors `crate::side_from_u8`'s encoding: `0` = Long/Bid, `1` = Short/Ask,
    // anything else is malformed and must map to `None` (the handler surfaces
    // `InvalidAccountData` for a corrupt `position.side`).
    #[test]
    fn position_side_from_side_u8_encoding() {
        assert_eq!(PositionSide::from_side_u8(0), Some(PositionSide::Long));
        assert_eq!(PositionSide::from_side_u8(1), Some(PositionSide::Short));
        // The on-chain `position.side` is a `u8`; every other byte is invalid.
        for invalid in [2u8, 3, 127, 128, 255] {
            assert_eq!(PositionSide::from_side_u8(invalid), None);
        }
    }

    // --- REQ-2 / REQ-8: equal components give the exact rate; normalize_sums
    // lands in the documented window ---

    #[test]
    fn equal_components_rate_is_exact() {
        let (n, d, w1, w2) = (7u64, 1_000_000u64, 1_000u64, 3_000u64);
        let (a, b) = accumulate_entry(0, 0, n, d, w1).unwrap();
        let (sn, sd) = accumulate_entry(a, b, n, d, w2).unwrap();
        // rate = sn/sd == n/d exactly  <=>  sn * d == n * sd (u128)
        assert_eq!(
            sn * (d as u128),
            (n as u128) * sd,
            "equal rates average exactly"
        );
    }

    #[test]
    fn normalize_sums_window_and_trivial_cases() {
        // degenerate -> (0, 0); tiny sums -> exact (k = 0)
        assert_eq!(normalize_sums(0, 0), (0, 0));
        assert_eq!(normalize_sums(1, 1), (1, 1));
        assert_eq!(
            normalize_sums(APY_SCALE as u128, APY_SCALE as u128),
            (APY_SCALE, APY_SCALE)
        );

        // large sums: the larger component lands in [2^44, 2^45)
        for shift in [45u32, 46, 60, 90] {
            let big = 1u128 << shift;
            let (n_e, d_e) = normalize_sums(big, big);
            assert!((1u128 << 44) <= n_e as u128 && (n_e as u128) < (1u128 << 45));
            assert!((1u128 << 44) <= d_e as u128 && (d_e as u128) < (1u128 << 45));
        }
        // u64::MAX sums (bitlen 64) -> k = 19 -> both land just below 2^45
        let (n_e, d_e) = normalize_sums(u128::from(u64::MAX), u128::from(u64::MAX));
        assert!((n_e as u128) >= (1u128 << 44) && (n_e as u128) < (1u128 << 45));
        assert!((d_e as u128) >= (1u128 << 44) && (d_e as u128) < (1u128 << 45));

        // asymmetric: 2^60 (bitlen 61) -> k = 16; the smaller 2^40 falls below
        // the window (2^24) but stays nonzero — the ratio is preserved.
        let (n_e, d_e) = normalize_sums(1u128 << 60, 1u128 << 40);
        assert_eq!(n_e, 1u64 << 44);
        assert_eq!(d_e, 1u64 << 24);
    }

    // DESIGN §5 window invariant as a property (the deterministic test above
    // pins specific shifts; this one drives every u128 pair). The LARGER
    // normalized component must land in [2^44, 2^45) whenever its bit length
    // exceeds 45, and be exact (`k = 0`) when both sums already fit in 45 bits.
    proptest! {
        #[test]
        fn normalize_sums_window_and_exactness(
            n in 0u128..u128::MAX,
            d in 0u128..u128::MAX,
        ) {
            let (an, ad) = normalize_sums(n, d);
            let max = n.max(d);
            let bitlen = u128::BITS - max.leading_zeros();
            if bitlen > 45 {
                // The LARGER normalized component lands in [2^44, 2^45).
                let larger = if n >= d { an } else { ad };
                prop_assert!(
                    (1u128 << 44) <= larger as u128 && (larger as u128) < (1u128 << 45),
                    "the larger sum must land in [2^44, 2^45) after the shared shift"
                );
            } else {
                // Exact (k = 0): both sums were already < 2^45, so the u64 cast
                // is lossless.
                prop_assert_eq!(an, n as u64, "exact normalization below 2^45");
                prop_assert_eq!(ad, d as u64, "exact normalization below 2^45");
            }
        }

        // I-pnl-sign at the tiny-rate-difference edge: pnl must never take the
        // WRONG sign when the current rate differs from the entry rate by less
        // than the normalization quantization — it must be 0 or carry the
        // correct sign (the quantization floor naturally protects the sign), and
        // long/short must remain exact opposites.
        #[test]
        fn pnl_sign_never_flips_at_tiny_rate_differences(
            base in 100_000_000_000_000u64..100_000_000_000_000_000u64,
            w in 1_000_000u64..1_000_000_000_000u64,
            notional in 1_000_000u64..1_000_000_000_000_000_000u64,
            dn in 0u64..1_000u64,
            db in 0u64..1_000u64,
        ) {
            let (n_sum, d_sum) = accumulate_entry(0, 0, base, base, w).unwrap();
            let cur_n = base.checked_add(dn).unwrap();
            let cur_d = base.checked_add(db).unwrap();
            let (ne, de) = normalize_sums(n_sum, d_sum);
            // `num` is the gate on `signed_yield_change`'s sign (cross-multiplied
            // current vs normalized-entry rate).
            let num = (cur_n as i128) * (de as i128) - (ne as i128) * (cur_d as i128);
            let p_long = pnl(n_sum, d_sum, cur_n, cur_d, notional, PositionSide::Long);
            let p_short = pnl(n_sum, d_sum, cur_n, cur_d, notional, PositionSide::Short);
            if let (Some(pl), Some(ps)) = (p_long, p_short) {
                prop_assert_eq!(pl, -ps, "long and short are exact opposites");
                if num > 0 {
                    prop_assert!(pl >= 0, "rate_cur > rate_entry must never produce a long loss");
                } else if num < 0 {
                    prop_assert!(pl <= 0, "rate_cur < rate_entry must never produce a long profit");
                } else {
                    prop_assert_eq!(pl, 0, "equal rates => zero pnl");
                }
            }
        }
    }

    // ==== Adversarial-review invariants for positions.rs (moved from review_tests.rs) ====
    // These independently pin the position-lifecycle contract (apply_pnl, pnl,
    // margin_required) across the FULL domain, so the implementation's own
    // in-file tests (which share its assumptions) cannot mask a counterexample.

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

    // settle_funding / settle_close adapter invariant: `apply_pnl` applied to the
    // signed payment / realized PnL preserves the "always a valid deposited
    // amount" postcondition for EVERY valid (deposited, signed_amount) pair.
    proptest! {
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
    }
}

// --- Issue #7: realized-yield settlement (R-S2, R-S3) -----------------------
//
// `close_position` stays lifecycle-only (D4) but records the closed notional in
// `Position.closed_notional`; a permissionless `settle_close` realizes the
// **signed** PnL (positions::pnl, trustless via the entry running sums) into the
// user's `UserCollateral.deposited`. `apply_pnl` is the pure ledger transition:
// positive PnL credits, negative PnL debits but is clamped so `deposited` never
// goes below 0 (the vault is never insolvent — R-S3).

#[cfg(test)]
mod settlement_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn zero_pnl_keeps_deposited(deposited in 0u64..1_000_000_000_000) {
            prop_assert_eq!(apply_pnl(deposited, 0), Some(deposited));
        }

        #[test]
        fn positive_pnl_credits(deposited in 0u64..1_000_000_000_000, pnl in 0i128..1_000_000_000_000) {
            let want = (deposited as i128) + pnl;
            if want <= u64::MAX as i128 {
                prop_assert_eq!(apply_pnl(deposited, pnl), Some(want as u64));
            } else {
                // Overflow returns None (checked add).
                prop_assert_eq!(apply_pnl(deposited, pnl), None);
            }
        }

        #[test]
        fn negative_pnl_debits_but_never_negative(
            deposited in 0u64..1_000_000_000_000,
            loss in 1i128..1_000_000_000_000,
        ) {
            let out = apply_pnl(deposited, -loss).expect("apply_pnl is total on u64 x i128");
            // Clamped at 0: the vault never goes negative (R-S3).
            if (loss as u64) <= deposited {
                prop_assert_eq!(out, deposited - (loss as u64));
            } else {
                prop_assert_eq!(out, 0, "loss clamped so deposited never negative");
            }
        }

        #[test]
        fn pnl_never_makes_deposited_negative(deposited in 0u64..1_000_000_000_000, pnl in -1_000_000_000_000i128..1_000_000_000_000) {
            let out = apply_pnl(deposited, pnl).unwrap();
            prop_assert!(out <= deposited || pnl > 0, "a loss never increases deposited");
        }
    }

    // R-S2/R-S3 boundary + extreme inputs the property above's ranges cannot
    // reach (the proptest ranges keep `want` far below `u64::MAX`, so the
    // positive-overflow branch is dead-code coverage). Pin the exact overflow
    // threshold and the clamp-at-zero extreme so the signed `apply_pnl` contract
    // is fully locked (never panics, never None on a loss, None only on a
    // positive overflow).
    #[test]
    fn apply_pnl_overflow_boundary_and_extremes() {
        // Positive credit exactly to u64::MAX succeeds; one more overflows -> None.
        assert_eq!(apply_pnl(0, u64::MAX as i128), Some(u64::MAX));
        assert_eq!(apply_pnl(1, (u64::MAX as i128) - 1), Some(u64::MAX));
        assert_eq!(
            apply_pnl(1, u64::MAX as i128),
            None,
            "a positive credit past u64::MAX is None (checked), never a wrap"
        );
        // A negative loss clamps at 0 and is never None (R-S3 never insolvent).
        assert_eq!(apply_pnl(0, -1), Some(0));
        assert_eq!(apply_pnl(5, -5), Some(0));
        assert_eq!(
            apply_pnl(5, -6),
            Some(0),
            "loss clamps to the deposited floor"
        );
        assert_eq!(
            apply_pnl(u64::MAX, i128::MIN),
            Some(0),
            "an extreme loss clamps deposited to 0"
        );
        assert_eq!(
            apply_pnl(u64::MAX, 0),
            Some(u64::MAX),
            "zero pnl is a no-op"
        );
    }

    // R-S2: settlement value is the index-based PnL (trustless), applied via
    // apply_pnl — positive net to a winner, negative (clamped) to a loser.
    #[test]
    fn settle_close_long_profit_credits_collateral() {
        // A long that profited on a rising index: entry (n0,d0) -> current higher.
        let n0 = 100_000_000_000_000u64;
        let d0 = 100_000_000_000_000u64;
        let w = 1_000_000u64;
        let notional = 5_000_000u64;
        let (n_sum, d_sum) = accumulate_entry(0, 0, n0, d0, w).unwrap();
        // Current rate higher (d1 = d0 / 2 => rate doubles).
        let d1 = d0 / 2;
        let pnl_long = pnl(n_sum, d_sum, n0, d1, notional, PositionSide::Long).unwrap();
        assert!(pnl_long > 0, "long profits when the index rises");
        let deposited = 100_000_000u64;
        assert_eq!(
            apply_pnl(deposited, pnl_long).unwrap(),
            deposited + pnl_long as u64
        );
    }

    #[test]
    fn settle_close_long_loss_debits_but_clamped() {
        let n0 = 100_000_000_000_000u64;
        let d0 = 100_000_000_000_000u64;
        let w = 1_000_000u64;
        let notional = 5_000_000u64;
        let (n_sum, d_sum) = accumulate_entry(0, 0, n0, d0, w).unwrap();
        // Current rate LOWER (n1 = n0 / 2 => rate halves) -> long loses.
        let n1 = n0 / 2;
        let pnl_long = pnl(n_sum, d_sum, n1, d0, notional, PositionSide::Long).unwrap();
        assert!(pnl_long < 0, "long loses when the index falls");
        let deposited = 1_000_000u64;
        let out = apply_pnl(deposited, pnl_long).unwrap();
        // Clamped so deposited never goes negative.
        assert!(out <= deposited);
    }
}
