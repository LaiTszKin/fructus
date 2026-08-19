//! Fructus — a Solana protocol for trading yield futures.
//!
//! This milestone implements the **data module**: an on-chain mark-price APY
//! oracle that is updated via publisher-signed (ed25519) data, with a
//! staleness predicate consumers can use as a circuit breaker.

use anchor_lang::prelude::*;

pub mod constants;
pub mod ed25519;
pub mod error;
pub mod exchange;
pub mod state;

use constants::{ORACLE_SEED, PERP_MARKET_SEED, VAULT_SEED};
use error::FructusError;
use exchange::{ExchangeRate, STAKE_POOL_PROGRAM_ID};
use state::{
    apy_in_bounds, funding_k_in_bounds, initial_margin_in_bounds, maintenance_margin_in_bounds,
    max_funding_in_bounds, update_message, validate_version, PerpMarket, YieldOracle,
};

/// Validate that `account` is owned by the SPL Stake Pool program and carries
/// the `StakePool` discriminator, returning the parsed [`ExchangeRate`].
fn read_stake_pool(account: &AccountInfo) -> Result<ExchangeRate> {
    require!(
        account.owner == &STAKE_POOL_PROGRAM_ID,
        FructusError::InvalidStakePool
    );
    ExchangeRate::read(&account.data.borrow()).ok_or(FructusError::InvalidStakePool.into())
}

declare_id!("8ZLiJ12eBiam4UP2HRp3M75CQAcc8GuUBz44zeHt6mjH");

#[program]
pub mod fructus {
    use super::*;

    /// Create the singleton yield oracle.
    pub fn initialize(
        ctx: Context<Initialize>,
        publisher: Pubkey,
        stale_after_slots: u64,
        initial_apy: u64,
    ) -> Result<()> {
        require!(apy_in_bounds(initial_apy), FructusError::ApyTooHigh);
        let oracle = &mut ctx.accounts.oracle;
        oracle.apy = initial_apy;
        oracle.version = 0;
        oracle.last_update_slot = Clock::get()?.slot;
        oracle.publisher = publisher;
        oracle.authority = ctx.accounts.authority.key();
        oracle.stale_after_slots = stale_after_slots;
        oracle.bump = ctx.bumps.oracle;
        Ok(())
    }

    /// Create the singleton perpetual market and bind it to a trustless index
    /// source (jitoSOL stake pool), a collateral mint, and funding/margin
    /// parameters. The collateral-vault PDA is derived and stored here but its
    /// token account is not created (deferred to a later issue).
    pub fn initialize_market(
        ctx: Context<InitializeMarket>,
        collateral_mint: Pubkey,
        funding_k: u64,
        max_funding: u64,
        funding_epoch_slots: u64,
        initial_margin_bps: u16,
        maintenance_margin_bps: u16,
    ) -> Result<()> {
        require!(
            funding_k_in_bounds(funding_k),
            FructusError::InvalidFundingK
        );
        require!(
            max_funding_in_bounds(max_funding),
            FructusError::InvalidMaxFunding
        );
        require!(
            initial_margin_in_bounds(initial_margin_bps),
            FructusError::InvalidInitialMargin
        );
        require!(
            maintenance_margin_in_bounds(initial_margin_bps, maintenance_margin_bps),
            FructusError::InvalidMaintenanceMargin
        );

        let index_source = &ctx.accounts.index_source;
        read_stake_pool(index_source)?;

        let vault = Pubkey::find_program_address(&[VAULT_SEED], &crate::ID).0;

        let market = &mut ctx.accounts.market;
        market.index_source = index_source.key();
        market.collateral_mint = collateral_mint;
        market.funding_k = funding_k;
        market.max_funding = max_funding;
        market.funding_epoch_slots = funding_epoch_slots;
        market.initial_margin_bps = initial_margin_bps;
        market.maintenance_margin_bps = maintenance_margin_bps;
        market.authority = ctx.accounts.authority.key();
        market.vault = vault;
        market.bump = ctx.bumps.market;
        Ok(())
    }

    /// Update the APY reference using a publisher-signed value.
    ///
    /// The transaction must carry an `ed25519` verify instruction whose public
    /// key is `oracle.publisher` and whose message is
    /// `update_message(oracle, apy, version)`.
    pub fn update_apy(ctx: Context<UpdateApy>, apy: u64, version: u64) -> Result<()> {
        require!(apy_in_bounds(apy), FructusError::ApyTooHigh);

        {
            let oracle = &ctx.accounts.oracle;
            let publisher = oracle.publisher;
            let oracle_key = oracle.key();
            let message = update_message(&oracle_key, apy, version);
            let ix_sysvar = ctx.accounts.instruction_sysvar.to_account_info();
            ed25519::verify_publisher_signature(&ix_sysvar, &publisher, &message)?;
        }

        {
            let oracle = &mut ctx.accounts.oracle;
            validate_version(oracle.version, version)?;
            oracle.apy = apy;
            oracle.version = version;
            oracle.last_update_slot = Clock::get()?.slot;
        }
        Ok(())
    }

    /// Change the staleness window (authority only).
    pub fn set_stale_window(ctx: Context<Admin>, new_stale_after_slots: u64) -> Result<()> {
        ctx.accounts.oracle.stale_after_slots = new_stale_after_slots;
        Ok(())
    }

    /// Rotate the publisher key (authority only).
    pub fn set_publisher(ctx: Context<Admin>, new_publisher: Pubkey) -> Result<()> {
        ctx.accounts.oracle.publisher = new_publisher;
        Ok(())
    }

    /// Derive the current exchange rate (SOL per pool token) from a stake pool
    /// account, on-chain and trustless.
    ///
    /// The rate is `total_lamports / pool_token_supply`, read directly from the
    /// pool account after validating that the account is owned by the SPL Stake
    /// Pool program and carries the `StakePool` discriminator. No external
    /// oracle or signed input is trusted.
    pub fn read_exchange_rate(ctx: Context<ReadExchangeRate>) -> Result<()> {
        let rate = read_stake_pool(&ctx.accounts.stake_pool)?;
        msg!(
            "fructus exchange_rate total_lamports={} pool_token_supply={}",
            rate.total_lamports,
            rate.pool_token_supply
        );
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + YieldOracle::LEN,
        seeds = [ORACLE_SEED],
        bump
    )]
    pub oracle: Account<'info, YieldOracle>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitializeMarket<'info> {
    #[account(
        init,
        payer = payer,
        space = 8 + PerpMarket::LEN,
        seeds = [PERP_MARKET_SEED],
        bump
    )]
    pub market: Account<'info, PerpMarket>,
    /// CHECK: the handler validates owner == SPL Stake Pool program and
    /// account_type == StakePool before using it as the index source.
    pub index_source: UncheckedAccount<'info>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateApy<'info> {
    #[account(mut, seeds = [ORACLE_SEED], bump = oracle.bump)]
    pub oracle: Account<'info, YieldOracle>,
    /// CHECK: the instruction sysvar, used to introspect the ed25519 verify
    /// instruction. `load_instruction_at_checked` rejects a non-sysvar account.
    pub instruction_sysvar: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct Admin<'info> {
    #[account(mut, seeds = [ORACLE_SEED], bump = oracle.bump, has_one = authority)]
    pub oracle: Account<'info, YieldOracle>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct ReadExchangeRate<'info> {
    /// CHECK: the handler validates the owner (SPL Stake Pool program) and the
    /// `account_type == StakePool` discriminator before reading the fields.
    pub stake_pool: UncheckedAccount<'info>,
}

#[cfg(test)]
mod tests;
