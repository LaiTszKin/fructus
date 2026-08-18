//! Fructus — a Solana protocol for trading yield futures.
//!
//! The protocol starts with jitoSOL yield perpetual futures (MVP), then adds
//! jitoSOL dated futures, and eventually expands to other yield-bearing assets.
//!
//! This is an initial scaffold: the program ID below is a placeholder and the
//! instruction surface is intentionally minimal. Replace the program ID with
//! your own keypair before deploying.

use anchor_lang::prelude::*;

// Program keypair is generated locally at `target/deploy/fructus-keypair.json`
// (gitignored). Keep it safe — it is required to upgrade the deployed program.
declare_id!("8ZLiJ12eBiam4UP2HRp3M75CQAcc8GuUBz44zeHt6mjH");

/// The Fructus on-chain program.
///
/// Instructions for yield perpetual and dated futures will be added here as
/// the protocol is implemented.
#[program]
pub mod fructus {
    use super::*;

    /// Placeholder initialization instruction.
    ///
    /// Establishes a program-owned state account; real market/position
    /// accounts will replace it in subsequent milestones.
    pub fn initialize(_ctx: Context<Initialize>) -> Result<()> {
        msg!("Fructus initialized");
        Ok(())
    }
}

/// Accounts required by [`initialize`](fructus::initialize).
#[derive(Accounts)]
pub struct Initialize {}
