//! Pure collateral-vault accounting: the free-collateral predicate and the
//! deposit/withdraw ledger transitions.
//!
//! This module is deliberately free of Anchor account plumbing (mirroring the
//! `exchange.rs` split): every function takes and returns plain `u64` values so
//! `proptest` can exercise the invariants directly, and the thin
//! `deposit_collateral` / `withdraw_collateral` adapters in `lib.rs` apply them
//! to the on-chain `UserCollateral` ledger.
//!
//! All arithmetic is `checked_*` — there is no panicking math, and no new
//! dependency is introduced. Amounts are USDC microunits (6 decimals).

/// The free-collateral predicate: `deposited - reserved`, the seam the position
/// lifecycle will later use to keep collateral backing open margin from being
/// withdrawn.
///
/// Returns `Some(free)` when `deposited >= reserved`, and `None` when
/// `reserved > deposited` — a ledger invariant violation (the three quantities
/// `deposited`, `reserved`, and `free` are never negative).
///
/// Because `reserved` is stubbed to `0` this iteration, this reduces to
/// `deposited`, which is exactly the `reserved == 0 => free_collateral ==
/// deposited` invariant.
pub fn free_collateral(deposited: u64, reserved: u64) -> Option<u64> {
    deposited.checked_sub(reserved)
}

/// Deposit transition: `deposited + amount`.
///
/// Returns `Some(new_deposited)` on success and `None` on `u64` overflow (the
/// caller maps this to the checked-arithmetic overflow error).
pub fn deposit(deposited: u64, amount: u64) -> Option<u64> {
    deposited.checked_add(amount)
}

/// Withdraw transition: `deposited - amount`, guarded by the free-collateral
/// seam.
///
/// Succeeds **only** when `amount <= free_collateral(deposited, reserved)`,
/// returning `Some(deposited - amount)`; otherwise (including `amount > free`,
/// or a `reserved > deposited` invariant violation) it returns `None` and the
/// caller leaves the ledger untouched. `amount == free` is allowed and returns
/// the remaining `deposited` (the reserved amount).
pub fn withdraw(deposited: u64, reserved: u64, amount: u64) -> Option<u64> {
    let free = free_collateral(deposited, reserved)?;
    if amount > free {
        return None;
    }
    deposited.checked_sub(amount)
}
