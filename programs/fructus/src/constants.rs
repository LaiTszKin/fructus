//! Protocol-wide constants for the yield oracle data module.

/// Fixed-point scaling for the stored APY value: `1.0 == APY_SCALE`.
pub const APY_SCALE: u64 = 1_000_000;

/// Sanity ceiling for a single APY value (100% == `APY_SCALE`).
pub const MAX_APY: u64 = APY_SCALE;

/// Domain-separator prefix for update signatures.
pub const UPDATE_DOMAIN_SEPARATOR: &[u8] = b"fructus::update_apy";

/// PDA seed for the singleton yield oracle account.
pub const ORACLE_SEED: &[u8] = b"yield_oracle";
