//! Pure settlement-pool logic (the "Design A" PnL pool + per-user pending
//! claims) — the counter-party netting layer that fixes the unbacked-collateral
//! mint in `settle_close` / `settle_funding` / `liquidate`.
//!
//! # Motivation (why this module exists)
//!
//! `positions::apply_pnl` is a **pure single-ledger** transition: a positive PnL
//! credits `deposited` unbounded, a negative PnL debits it but is clamped at
//! zero. It is deliberately unchanged (see `positions.rs`) — it is *not* the bug.
//! The bug is wiring that function directly onto a winner's ledger with **no
//! counterparty netting**: a winner is credited in full while an
//! under-collateralized loser's debit is silently clamped away, so the sum of
//! `UserCollateral.deposited` across users can exceed the vault's real USDC
//! holdings (unbacked, withdrawable collateral).
//!
//! Design A fixes this with a **market-level PnL pool** plus **per-user pending
//! claims**:
//!
//! * a **loser**'s debit is *collected* into the pool (`apply_debit`), bounded by
//!   the loser's actual `deposited` (the same clamp `apply_pnl` uses — no
//!   over-draw);
//! * a **winner**'s credit is *paid* only up to what the pool actually holds
//!   (`apply_credit`); the unfunded remainder becomes a **pending claim**
//!   (`claimable`), never `deposited`, so it is never withdrawable until future
//!   losses fund it (`claim_payout`).
//!
//! Every function is pure, `Option`-returning, and `checked_*`/`saturating_*` —
//! no panicking math (AGENTS.md). The invariant is exact and property-tested:
//! **total payments ≤ total collections** (equivalently `pool ≥ 0`), so
//! `Σ deposited` never exceeds the value actually entered and collected.

/// Apply a **loser-side debit** (a realized loss or a funding payment a payer
/// owes). This is the "collect the loss into the pool" side of Design A.
///
/// * `collected = min(debit, deposited)` — the same clamp `positions::apply_pnl`
///   uses on its debit side (the loser is never over-drawn);
/// * `new_deposited = deposited - collected`;
/// * `new_pool = pool + collected` (`None` only on a `u64` overflow).
///
/// Returns `(new_deposited, new_pool)`.
pub fn apply_debit(deposited: u64, pool: u64, debit: u64) -> Option<(u64, u64)> {
    let collected = debit.min(deposited);
    let new_deposited = deposited - collected;
    let new_pool = pool.checked_add(collected)?;
    Some((new_deposited, new_pool))
}

/// Apply a **winner-side credit** against the pool (Design A).
///
/// * `paid = min(credit, pool)` — a winner is paid only up to what the pool
///   actually collected from losers; **never minted**;
/// * `new_deposited = deposited + paid` (`None` on `u64` overflow);
/// * `new_pool = pool - paid`;
/// * `new_claimable = claimable + (credit - paid)` — the unfunded remainder is
///   recorded as a **pending claim**, not `deposited`.
///
/// Returns `(new_deposited, new_claimable, new_pool)`.
pub fn apply_credit(
    deposited: u64,
    claimable: u64,
    pool: u64,
    credit: u64,
) -> Option<(u64, u64, u64)> {
    let paid = credit.min(pool);
    let new_deposited = deposited.checked_add(paid)?;
    let new_pool = pool - paid;
    let new_claimable = claimable.checked_add(credit - paid)?;
    Some((new_deposited, new_claimable, new_pool))
}

/// Convert a pending claim into deposited collateral **up to the pool's actual
/// holdings** (Design A). This is the only path by which `claimable` becomes
/// withdrawable `deposited` (run at the start of deposit/withdraw).
///
/// * `pay = min(claimable, pool)`;
/// * `new_deposited = deposited + pay` (`None` on `u64` overflow);
/// * `new_claimable = claimable - pay`;
/// * `new_pool = pool - pay`.
///
/// Returns `(new_deposited, new_claimable, new_pool)`.
pub fn claim_payout(deposited: u64, claimable: u64, pool: u64) -> Option<(u64, u64, u64)> {
    let pay = claimable.min(pool);
    let new_deposited = deposited.checked_add(pay)?;
    let new_claimable = claimable - pay;
    let new_pool = pool - pay;
    Some((new_deposited, new_claimable, new_pool))
}

/// Book a liquidated **victim's realized loss** into the pool (Design A) — the
/// loss a losing position actually incurred at liquidation, so the loser is
/// collected (never vanishes as a counterparty).
///
/// The booked loss is capped at the victim's **free seam plus the position's
/// released margin**, and *after* the liquidator reward is reserved:
///
/// * `booked = min(loss, deposited - reserved_after - reward)`;
/// * `new_deposited = deposited - booked`.
///
/// `reserved_after` is the victim's reserved collateral **after** releasing this
/// position's backing (i.e. the collateral still backing *other* positions), so
/// the booked loss never eats into other positions' reserved backing, and the
/// `reward` is payable first (no underflow). Returns `(new_deposited, booked)`.
pub fn apply_liquidation_loss(
    deposited: u64,
    reserved_after: u64,
    loss: u64,
    reward: u64,
) -> Option<(u64, u64)> {
    let seam = deposited.checked_sub(reserved_after)?.checked_sub(reward)?;
    let booked = loss.min(seam);
    Some((deposited - booked, booked))
}

/// Route a **signed** PnL / funding payment through the pool (Design A), with
/// the exact routing the handlers use:
///
/// * `signed_pnl <= 0` ⇒ `apply_debit(deposited, pool, |signed_pnl|)` — the
///   loser's debit is collected into the pool (clamped at `deposited`);
/// * `signed_pnl > 0` ⇒ `apply_credit(deposited, claimable, pool, signed_pnl)` —
///   the winner is paid only up to the pool, the remainder becomes a claim.
///
/// Returns `(new_deposited, new_claimable, new_pool)`; `None` only on a genuine
/// `u64` overflow (positive credit past `u64::MAX`, `deposited + paid` overflow,
/// `claimable + remainder` overflow, or `pool + collected` overflow).
pub fn settle_signed(
    deposited: u64,
    claimable: u64,
    pool: u64,
    signed_pnl: i128,
) -> Option<(u64, u64, u64)> {
    if signed_pnl <= 0 {
        // Saturate the magnitude to `u64`; `apply_debit` then clamps at
        // `deposited` exactly like `positions::apply_pnl`'s debit side.
        let debit = signed_pnl.unsigned_abs().min(u64::MAX as u128) as u64;
        let (new_deposited, new_pool) = apply_debit(deposited, pool, debit)?;
        Some((new_deposited, claimable, new_pool))
    } else {
        let credit = u64::try_from(signed_pnl).ok()?;
        apply_credit(deposited, claimable, pool, credit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // =========================================================================
    // INVARIANT 1 — `pool_never_negative_under_any_op_sequence`.
    //
    // Random sequences of apply_debit / apply_credit / claim_payout /
    // apply_liquidation_loss over random states: the pool never goes negative,
    // deposited/claimable never underflow (type-guaranteed), and the returned op
    // is total on its domain (None only on a genuine u64 overflow, never a panic
    // or an underflow). We track the running accounting identity
    //   `pool == total_collected - total_paid`   (u128)
    // which proves `pool >= 0` and, equivalently, **total payments ≤ total
    // collections** — the anti-mint guarantee.
    // =========================================================================

    #[derive(Clone, Copy, Debug)]
    enum Op {
        Debit(u64),
        Credit(u64),
        Claim,
        Liquidation {
            reserved_after: u64,
            loss: u64,
            reward: u64,
        },
    }

    fn step(
        deposited: &mut u64,
        claimable: &mut u64,
        pool: &mut u64,
        collected: &mut u128,
        paid: &mut u128,
        op: Op,
    ) -> Option<()> {
        match op {
            Op::Debit(debit) => {
                let (d2, p2) = apply_debit(*deposited, *pool, debit)?;
                let delta = *deposited - d2; // collected = min(debit, deposited)
                *deposited = d2;
                *pool = p2;
                *collected = collected.checked_add(delta as u128)?;
            }
            Op::Credit(credit) => {
                let (d2, c2, p2) = apply_credit(*deposited, *claimable, *pool, credit)?;
                let delta = d2 - *deposited; // paid = min(credit, pool)
                *deposited = d2;
                *claimable = c2;
                *pool = p2;
                *paid = paid.checked_add(delta as u128)?;
            }
            Op::Claim => {
                let (d2, c2, p2) = claim_payout(*deposited, *claimable, *pool)?;
                let delta = d2 - *deposited; // pay = min(claimable, pool)
                *deposited = d2;
                *claimable = c2;
                *pool = p2;
                *paid = paid.checked_add(delta as u128)?;
            }
            Op::Liquidation {
                reserved_after,
                loss,
                reward,
            } => {
                let (d2, booked) =
                    apply_liquidation_loss(*deposited, reserved_after, loss, reward)?;
                *deposited = d2;
                *pool = pool.checked_add(booked)?;
                *collected = collected.checked_add(booked as u128)?;
            }
        }
        Some(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        #[test]
        fn pool_never_negative_under_any_op_sequence(
            ops in proptest::collection::vec(any::<(u8, u64, u64, u64)>(), 0..64),
            d0 in any::<u64>(),
            c0 in any::<u64>(),
            p0 in any::<u64>(),
        ) {
            let mut deposited = d0;
            let mut claimable = c0;
            let mut pool = p0;
            // seed the accounting identity for a nonzero starting pool: treat the
            // starting pool as a prior collection (so `pool == collected - paid`
            // still holds; it only strengthens the invariant).
            let mut collected: u128 = p0 as u128;
            let mut paid: u128 = 0;

            for (tag, a, b, c) in ops {
                let op = match tag % 4 {
                    0 => Op::Debit(a),
                    1 => Op::Credit(a),
                    2 => Op::Claim,
                    _ => Op::Liquidation {
                        reserved_after: a,
                        loss: b,
                        reward: c,
                    },
                };
                // Totality: the op must not panic. `None` is allowed only as a
                // genuine u64 overflow (we simply skip the op in that case).
                let _ = step(&mut deposited, &mut claimable, &mut pool, &mut collected, &mut paid, op);
                prop_assert_eq!(
                    pool as u128,
                    collected - paid,
                    "pool must equal total_collected - total_paid (=> pool >= 0 and payments <= collections)"
                );
            }
        }
    }

    // =========================================================================
    // INVARIANT 2 — `system_value_conserved_no_mint`.
    //
    // Model the vault separately from the ledgers. Start `vault = Σ deposited`;
    // deposit/withdraw move both together; debit/credit/claim move only the
    // ledger + pool (the USDC stays in the vault). The hard invariants:
    //   (a) `pool >= 0` always;
    //   (b) `vault >= deposited` always — no minted, unbacked collateral;
    //   (c) total payments ≤ total collections (never double-pay).
    // =========================================================================

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        #[test]
        fn system_value_conserved_no_mint(
            ops in proptest::collection::vec(any::<(u8, u64)>(), 0..64),
            d0 in 0u64..1_000_000_000_000u64,
        ) {
            let mut deposited = d0;
            let mut claimable = 0u64;
            let mut pool = 0u64;
            let mut vault = d0;
            let mut collected: u128 = 0;
            let mut paid: u128 = 0;

            for (tag, amount) in ops {
                match tag % 5 {
                    // deposit: vault and deposited move together (+a).
                    0 => {
                        if let (Some(v), Some(d)) =
                            (vault.checked_add(amount), deposited.checked_add(amount))
                        {
                            vault = v;
                            deposited = d;
                        }
                    }
                    // withdraw: vault and deposited move together (-a), clamped to
                    // the deposited balance (no reserved seam in this model).
                    1 => {
                        let a = amount.min(deposited);
                        vault -= a;
                        deposited -= a;
                    }
                    // debit (loss collected): deposited -= collected, pool += collected.
                    2 => {
                        if let Some((d, p)) = apply_debit(deposited, pool, amount) {
                            collected += (deposited - d) as u128;
                            deposited = d;
                            pool = p;
                        }
                    }
                    // credit (winner paid from pool).
                    3 => {
                        if let Some((d, c, p)) = apply_credit(deposited, claimable, pool, amount) {
                            paid += (d - deposited) as u128;
                            deposited = d;
                            claimable = c;
                            pool = p;
                        }
                    }
                    // claim payout.
                    _ => {
                        if let Some((d, c, p)) = claim_payout(deposited, claimable, pool) {
                            paid += (d - deposited) as u128;
                            deposited = d;
                            claimable = c;
                            pool = p;
                        }
                    }
                }

                // (a) pool is non-negative (type-guaranteed; asserted explicitly).
                prop_assert!(pool >= 0);
                // (b) the vault always backs every deposited unit.
                prop_assert!(
                    vault >= deposited,
                    "vault {} must cover Σ deposited {} (no mint)",
                    vault,
                    deposited
                );
                // (c) payments never exceed collections (equivalently pool ==
                // collected - paid, so the pool is always non-negative).
                prop_assert_eq!(pool as u128, collected - paid, "total payments <= total collections");
            }
        }
    }

    // =========================================================================
    // INVARIANT 3 — `self_pairing_nets_to_zero`.
    //
    // A long and a short with IDENTICAL entry basis and notional carry exactly
    // opposite PnL (`positions::pnl` is antisymmetric). Settling the exact
    // opposite pair through the pool must net to zero when the loser is fully
    // collateralized (`deposited >= |loss|`): the loser's full debit funds the
    // winner's full credit, `claimable` stays `0`, and Σ(deposited + claimable)
    // is unchanged from before the pair's entry — no mint, no burn.
    // =========================================================================

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        #[test]
        fn self_pairing_nets_to_zero(
            d_long in 0u64..1_000_000_000_000u64,
            // The magnitude of the opposite PnL the pair realizes.
            pnl_mag in 0u64..1_000_000_000_000u64,
            // Extra collateral so the short is ALWAYS fully collateralized
            // (`d_short = pnl_mag + deficit >= pnl_mag`) — no rejects.
            deficit in 0u64..1_000_000_000_000u64,
        ) {
            let d_short = pnl_mag + deficit;

            let sum_before = (d_long as u128) + (d_short as u128);
            // Loser (short) debits first: its loss is collected into the pool.
            let (d_short_after, pool) = apply_debit(d_short, 0, pnl_mag).unwrap();
            // Winner (long) credits: paid from the pool.
            let (d_long_after, claimable_after, pool_after) =
                apply_credit(d_long, 0, pool, pnl_mag).unwrap();

            prop_assert_eq!(d_short_after, d_short - pnl_mag, "loser paid the full loss");
            prop_assert_eq!(d_long_after, d_long + pnl_mag, "winner received the full profit");
            prop_assert_eq!(claimable_after, 0, "fully-funded pair leaves no pending claim");
            prop_assert_eq!(pool_after, 0, "pool drains back to zero");
            prop_assert_eq!(
                (d_long_after as u128) + (d_short_after as u128) + (claimable_after as u128),
                sum_before,
                "self-paired opposite position nets to zero (no mint, no burn)"
            );
        }
    }

    // =========================================================================
    // Deterministic minimal regressions (purpose-named).
    // =========================================================================

    /// The user-facing guarantee of Design A: a pending claim is converted into
    /// withdrawable `deposited` **only** up to the pool's actual holdings; the
    /// unfunded remainder stays a claim.
    #[test]
    fn claim_payout_converts_only_funded_claims() {
        // 100 claimed, but only 40 in the pool.
        let (deposited, claimable, pool) = claim_payout(0, 100, 40).unwrap();
        assert_eq!(deposited, 40, "only the funded 40 becomes deposited");
        assert_eq!(claimable, 60, "the unfunded 60 stays a pending claim");
        assert_eq!(pool, 0, "the pool is drained");
        // A fully-funded claim converts entirely.
        let (d2, c2, p2) = claim_payout(10, 100, 100).unwrap();
        assert_eq!(
            (d2, c2, p2),
            (110, 0, 0),
            "fully-funded claim converts fully"
        );
        // An empty pool converts nothing.
        let (d3, c3, p3) = claim_payout(10, 100, 0).unwrap();
        assert_eq!(
            (d3, c3, p3),
            (10, 100, 0),
            "empty pool leaves the claim pending"
        );
    }

    /// The user requirement: an exact-opposite long/short pair (identical
    /// basis/notional) settles through the pool to a **net zero** change in
    /// Σ(deposited + claimable) when the loser is fully collateralized.
    #[test]
    fn self_paired_long_short_nets_to_zero() {
        // Opposite PnL magnitude: a 100-USDC notional doubling in index => 100 USDC.
        let pnl_mag = 100_000_000u64;
        let (d_long, d_short) = (10_000_000u64, 150_000_000u64); // short fully funded
        let sum_before = d_long as u128 + d_short as u128;

        let (d_short_after, pool) = apply_debit(d_short, 0, pnl_mag).unwrap();
        let (d_long_after, claimable_after, pool_after) =
            apply_credit(d_long, 0, pool, pnl_mag).unwrap();

        assert_eq!(d_short_after, d_short - pnl_mag, "loser paid the full loss");
        assert_eq!(
            d_long_after,
            d_long + pnl_mag,
            "winner received the full profit"
        );
        assert_eq!(
            claimable_after, 0,
            "fully-funded pair leaves no pending claim"
        );
        assert_eq!(pool_after, 0, "pool nets back to zero");
        assert_eq!(
            (d_long_after as u128) + (d_short_after as u128) + (claimable_after as u128),
            sum_before,
            "self-paired long+short nets to zero (no mint, no burn)"
        );
    }

    /// The liquidated victim's realized loss is booked into the pool, bounded by
    /// the victim's free seam + this position's released margin **after** the
    /// reward — never into other positions' reserved backing.
    #[test]
    fn liquidation_books_loss_into_pool_without_touching_other_reserved() {
        let deposited = 1_000_000u64;
        let reserved_after = 300_000u64; // backing for the victim's OTHER positions
        let reward = 50_000u64; // liquidator reward (payable first)
        let loss = 900_000u64; // index-based realized loss

        let (new_deposited, booked) =
            apply_liquidation_loss(deposited, reserved_after, loss, reward).unwrap();
        // seam = 1_000_000 - 300_000 - 50_000 = 650_000 => booked capped there.
        assert_eq!(
            booked, 650_000,
            "loss booked only against the free seam + released margin - reward"
        );
        assert_eq!(
            new_deposited,
            deposited - booked,
            "deposited debited by exactly the booked loss"
        );
        assert!(
            new_deposited - reward >= reserved_after,
            "reward + booked never breach the remaining reserved backing"
        );

        // A small loss books exactly.
        let (nd, b) = apply_liquidation_loss(deposited, reserved_after, 100_000, reward).unwrap();
        assert_eq!(
            (b, nd),
            (100_000, deposited - 100_000),
            "small loss books exactly"
        );

        // An insolvent seam (reserved_after + reward > deposited) books nothing and
        // must be reported as None (the caller rejects the transition).
        assert_eq!(
            apply_liquidation_loss(100, 80, 1_000, 30),
            None,
            "insolvent seam is None"
        );
    }

    /// The OLD (buggy) behaviour, pinned for reference only — never re-enabled:
    ///
    /// ```ignore
    /// // BEFORE [fix A]: winner credited in FULL, loser clamped at 0, no pool.
    /// let d_long_after = positions::apply_pnl(d_long, p_long).unwrap(); // +100_000_000
    /// let d_short_after = positions::apply_pnl(d_short, p_short).unwrap(); // 0
    /// // Σ deposited grows 20_000_000 -> 120_000_000: 100_000_000 unbacked USDC minted.
    /// ```
    #[test]
    fn old_buggy_behaviour_is_not_restored() {
        // The fixed routing (via settle_signed) must NOT reproduce the mint: a
        // winner with an under-collateralized counterparty is paid only the
        // collected pool, the remainder becoming a claim.
        let (d_long, d_short) = (10_000_000u64, 10_000_000u64);
        let pnl_mag = 100_000_000u64; // winner +100m, loser -100m (only 10m posted)
        let (d_short_after, pool) = apply_debit(d_short, 0, pnl_mag).unwrap();
        let (d_long_after, claimable_after, pool_after) =
            apply_credit(d_long, 0, pool, pnl_mag).unwrap();
        assert_eq!(d_short_after, 0, "loser clamped at 0 (10m collected)");
        assert_eq!(
            d_long_after, 20_000_000,
            "winner paid only the 10m collected (no mint)"
        );
        assert_eq!(
            claimable_after, 90_000_000,
            "unfunded remainder is a pending claim"
        );
        assert_eq!(pool_after, 0, "pool drained");
        assert_eq!(
            (d_long_after as u128) + (d_short_after as u128),
            (d_long as u128) + (d_short as u128),
            "Σ deposited is conserved — the [fix A] invariant"
        );
    }

    /// `settle_signed` routes a zero PnL as an identity and is exact-opposite
    /// consistent with `apply_debit`/`apply_credit`.
    #[test]
    fn settle_signed_routes_by_sign() {
        // zero => identity.
        assert_eq!(settle_signed(10, 3, 5, 0), Some((10, 3, 5)));
        // negative => debit (collect into pool).
        assert_eq!(settle_signed(10, 0, 0, -4), Some((6, 0, 4)));
        // positive => credit (paid up to pool).
        assert_eq!(settle_signed(10, 0, 4, 100), Some((14, 96, 0)));
        // positive past u64::MAX => None (checked, never a wrap).
        assert_eq!(settle_signed(10, 0, 0, (u64::MAX as i128) + 1), None);
    }
}
