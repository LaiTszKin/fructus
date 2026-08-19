//! Protocol-wide constants for the yield oracle data module.

/// Fixed-point scaling for the stored APY value: `1.0 == APY_SCALE`.
pub const APY_SCALE: u64 = 1_000_000;

/// Sanity ceiling for a single APY value (100% == `APY_SCALE`).
pub const MAX_APY: u64 = APY_SCALE;

/// Domain-separator prefix for update signatures.
pub const UPDATE_DOMAIN_SEPARATOR: &[u8] = b"fructus::update_apy";

/// PDA seed for the singleton yield oracle account.
pub const ORACLE_SEED: &[u8] = b"yield_oracle";

/// PDA seed for the singleton perpetual market account.
pub const PERP_MARKET_SEED: &[u8] = b"perp_market";

/// PDA seed for the collateral-vault account (derived, not created at init).
pub const VAULT_SEED: &[u8] = b"vault";

/// Lower bound (inclusive) for the funding convergence-speed parameter `funding_k`.
pub const FUNDING_K_MIN: u64 = 1;

/// Upper bound (inclusive) for `funding_k`, fixed-point scaled by `APY_SCALE`.
pub const FUNDING_K_MAX: u64 = APY_SCALE;

/// Upper bound (inclusive) for the per-epoch funding-rate cap `max_funding`.
pub const MAX_FUNDING_MAX: u64 = APY_SCALE;

/// Upper bound (inclusive) for margin ratios, expressed in basis points.
pub const MAX_MARGIN_BPS: u16 = 10_000;
