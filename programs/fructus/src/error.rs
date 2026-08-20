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
    #[msg("Order book is at capacity")]
    BookFull,
    #[msg("Order book is already initialized")]
    BookAlreadyInitialized,
    #[msg("Order book is not initialized")]
    BookNotInitialized,
    #[msg("Order price is invalid")]
    InvalidPrice,
    #[msg("Order size is invalid")]
    InvalidSize,
    #[msg("Order not found")]
    OrderNotFound,
    #[msg("Order owner mismatch")]
    OrderOwnerMismatch,
    #[msg("Self-trade is not allowed")]
    SelfTrade,
    #[msg("Collateral mint is invalid")]
    InvalidMint,
    #[msg("Insufficient free collateral")]
    InsufficientFreeCollateral,
    #[msg("Collateral vault is already initialized")]
    VaultAlreadyInitialized,
    #[msg("Collateral vault is not initialized")]
    VaultNotInitialized,
    #[msg("Arithmetic overflow")]
    ArithmeticOverflow,
}
