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
}
