//! Error codes for the Fructus yield oracle.

use anchor_lang::prelude::*;

#[error_code]
pub enum FructusError {
    #[msg("APY exceeds the maximum allowed value")]
    ApyTooHigh,
    #[msg("Update version must be strictly greater than the current version")]
    StaleVersion,
    #[msg("Publisher signature is invalid")]
    InvalidSignature,
    #[msg("Publisher signature is missing from the transaction")]
    SignatureMissing,
    #[msg("Account is not a valid SPL stake pool")]
    InvalidStakePool,
    #[msg("Funding convergence speed (funding_k) is outside the allowed range")]
    InvalidFundingK,
    #[msg("Per-epoch funding rate cap (max_funding) is outside the allowed range")]
    InvalidMaxFunding,
    #[msg("Initial margin (basis points) is outside the allowed range")]
    InvalidInitialMargin,
    #[msg("Maintenance margin (basis points) must be positive and no greater than initial margin")]
    InvalidMaintenanceMargin,
}
