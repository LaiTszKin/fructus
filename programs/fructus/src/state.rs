//! Yield oracle account state and its pure, property-testable helpers.

use anchor_lang::prelude::*;
use sha2::{Digest, Sha256};

use crate::constants::{MAX_APY, UPDATE_DOMAIN_SEPARATOR};
use crate::error::FructusError;

/// On-chain mark-price APY reference for the yield oracle.
///
/// Stored as a singleton PDA (see [`crate::constants::ORACLE_SEED`]). Consumers
/// read `apy` together with `is_stale(..)` to decide whether the value is fresh
/// enough to use (circuit breaker).
#[account]
pub struct YieldOracle {
    /// Annualized yield, fixed-point scaled by `APY_SCALE` (1.0 == 1_000_000).
    pub apy: u64,
    /// Monotonic counter; strictly increases on every accepted update.
    pub version: u64,
    /// Slot at which the last accepted update was applied.
    pub last_update_slot: u64,
    /// Pubkey authorized to sign APY updates (verified via ed25519).
    pub publisher: Pubkey,
    /// Admin authority for config changes (stale window, publisher rotation).
    pub authority: Pubkey,
    /// Staleness window in slots; consumers treat the oracle as stale beyond it.
    pub stale_after_slots: u64,
    /// PDA bump seed.
    pub bump: u8,
}

impl YieldOracle {
    /// Serialized size of the account payload (excluding the 8-byte discriminator).
    pub const LEN: usize = 8 + 8 + 8 + 32 + 32 + 8 + 1;
}

/// Pure staleness predicate (saturating, overflow-safe for any `u64` inputs).
///
/// `is_stale(last, window, cur) == cur.saturating_sub(last) >= window`.
pub fn is_stale(last_update_slot: u64, stale_after_slots: u64, current_slot: u64) -> bool {
    current_slot.saturating_sub(last_update_slot) >= stale_after_slots
}

/// Whether an APY value lies within `[0, MAX_APY]`.
pub fn apy_in_bounds(apy: u64) -> bool {
    apy <= MAX_APY
}

/// Validate a version bump: the new version must be strictly greater.
pub fn validate_version(current: u64, next: u64) -> Result<()> {
    require!(next > current, FructusError::StaleVersion);
    Ok(())
}

/// Canonical 32-byte message the publisher signs for an update.
///
/// `sha256(domain_separator ‖ oracle_address ‖ apy_le ‖ version_le)`.
pub fn update_message(oracle: &Pubkey, apy: u64, version: u64) -> [u8; 32] {
    let mut buf = Vec::with_capacity(UPDATE_DOMAIN_SEPARATOR.len() + 32 + 8 + 8);
    buf.extend_from_slice(UPDATE_DOMAIN_SEPARATOR);
    buf.extend_from_slice(oracle.as_ref());
    buf.extend_from_slice(&apy.to_le_bytes());
    buf.extend_from_slice(&version.to_le_bytes());

    let digest = Sha256::digest(&buf);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}
