//! Trustless settlement: read the JitoSOL exchange rate and derive realized yield.
//!
//! jitoSOL is an SPL Stake Pool whose accrued value is entirely captured by the
//! on-chain exchange rate `total_lamports / pool_token_supply`. Because that
//! rate lives in the pool account itself (not in an oracle), it cannot go stale
//! and cannot be manipulated — the ideal settlement reference for yield futures.

use anchor_lang::prelude::*;

use crate::constants::APY_SCALE;

/// Byte offset of the borsh `AccountType` discriminator in the `StakePool`
/// account. Live `spl-stake-pool` accounts prepend this enum before the fields.
pub const ACCOUNT_TYPE_OFFSET: usize = 0;

/// `AccountType::StakePool` borsh discriminator (byte 0 of a live pool account).
pub const ACCOUNT_TYPE_STAKE_POOL: u8 = 1;

/// Byte offset of `total_lamports` in the SPL Stake Pool `StakePool` account
/// (borsh layout **with** the `account_type` discriminator prepended).
pub const TOTAL_LAMPORTS_OFFSET: usize = 258;

/// Byte offset of `pool_token_supply` in the SPL Stake Pool `StakePool` account.
pub const POOL_TOKEN_SUPPLY_OFFSET: usize = 266;

/// Canonical SPL Stake Pool program id (`SPoo1Ku8WFXoNDMHPsrGSTSG1Y47rzgn41SLUNakuHy`).
///
/// Used to prove an account passed to [`crate::fructus::read_exchange_rate`] is
/// genuinely owned by the stake pool program, so the derived rate is trustless.
pub const STAKE_POOL_PROGRAM_ID: Pubkey = Pubkey::new_from_array([
    6, 129, 78, 212, 202, 246, 138, 23, 70, 114, 253, 172, 134, 3, 26, 99, 232, 78, 161, 94, 250,
    29, 68, 183, 34, 147, 246, 219, 219, 0, 22, 80,
]);

/// A rational exchange rate (SOL per jitoSOL) read from the stake pool account.
///
/// Kept as a numerator/denominator pair to avoid precision loss until the final
/// yield computation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExchangeRate {
    /// Total lamports held by the pool.
    pub total_lamports: u64,
    /// Total pool token (jitoSOL) supply.
    pub pool_token_supply: u64,
}

impl ExchangeRate {
    /// Read the exchange rate from the raw stake pool account data.
    ///
    /// Returns `None` unless the account carries the `StakePool` discriminator,
    /// for data too short to contain both fields, or for a zero token supply.
    pub fn read(data: &[u8]) -> Option<Self> {
        if data.get(ACCOUNT_TYPE_OFFSET).copied() != Some(ACCOUNT_TYPE_STAKE_POOL) {
            return None;
        }
        let total_lamports = read_u64_le(data, TOTAL_LAMPORTS_OFFSET)?;
        let pool_token_supply = read_u64_le(data, POOL_TOKEN_SUPPLY_OFFSET)?;
        if pool_token_supply == 0 {
            return None;
        }
        Some(Self {
            total_lamports,
            pool_token_supply,
        })
    }

    /// Realized yield from `self` (open, t0) to `other` (settle, t1), scaled by
    /// [`APY_SCALE`].
    ///
    /// Returns `(rate_t1 / rate_t0 - 1) * APY_SCALE` using `u128` arithmetic to
    /// avoid intermediate overflow. A negative yield clamps to `0`. Returns
    /// `None` on a zero numerator/denominator or on overflow.
    pub fn realized_yield(&self, other: &Self) -> Option<u64> {
        if other.total_lamports == 0 {
            return None;
        }
        // rate_t1 / rate_t0 = (n1 * d0) / (n0 * d1)
        let a = (other.total_lamports as u128) * (self.pool_token_supply as u128);
        let b = (self.total_lamports as u128) * (other.pool_token_supply as u128);
        if b == 0 {
            return None;
        }
        if a < b {
            // Negative yield cannot happen for a functioning LST; clamp defensively.
            return Some(0);
        }
        // `a >= b` is guaranteed above, so plain subtraction cannot underflow.
        let diff = a - b;
        let scaled = diff.checked_mul(APY_SCALE as u128)?.checked_div(b)?;
        u64::try_from(scaled).ok()
    }
}

/// Annualize a scaled yield over `period_slots` elapsed slots.
///
/// `yield_scaled * slots_per_year / period_slots`, with `u128` intermediates.
/// Returns `None` for a zero period or overflow.
pub fn annualize(yield_scaled: u64, period_slots: u64, slots_per_year: u64) -> Option<u64> {
    if period_slots == 0 {
        return None;
    }
    let out = (yield_scaled as u128)
        .checked_mul(slots_per_year as u128)?
        .checked_div(period_slots as u128)?;
    u64::try_from(out).ok()
}

fn read_u64_le(data: &[u8], offset: usize) -> Option<u64> {
    let bytes = data.get(offset..offset.checked_add(8)?)?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}
