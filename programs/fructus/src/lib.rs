//! Fructus — a Solana protocol for trading yield futures.
//!
//! This milestone implements the **data module**: an on-chain mark-price APY
//! oracle that is updated via publisher-signed (ed25519) data, with a
//! staleness predicate consumers can use as a circuit breaker.

use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_spl::token::{self, Mint, Token, TokenAccount};

pub mod collateral;
pub mod constants;
pub mod ed25519;
pub mod error;
pub mod exchange;
pub mod orderbook;
pub mod state;

use constants::{
    EVENT_QUEUE_LEN, MAX_MATCH_STEPS, MAX_ORDERS_PER_SIDE, ORACLE_SEED, ORDER_BOOK_SEED,
    PERP_MARKET_SEED, TWAP_OBSERVATIONS, USDC_DECIMALS, USER_COLLATERAL_SEED, VAULT_SEED,
};
use error::FructusError;
use exchange::{ExchangeRate, STAKE_POOL_PROGRAM_ID};
use state::{
    apy_in_bounds, funding_k_in_bounds, initial_margin_in_bounds, maintenance_margin_in_bounds,
    max_funding_in_bounds, update_message, validate_version, OrderBook, OutEvent, PerpMarket,
    UserCollateral, YieldOracle,
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

// --- Order-book adapters (issue #3) --------------------------------------
//
// The on-chain `OrderBook` account stores fixed-capacity arrays (with an `active`
// flag per slot), while the pure `orderbook::Book` engine works over `Vec`s. The
// handlers below load the account into the pure model, run the engine, and write
// the result back — recording a TWAP observation and appending an `OutEvent` on
// every book mutation.

/// `OutEvent.kind` byte values (see `state::OutEvent`).
const EVENT_KIND_FILL: u8 = 0;
const EVENT_KIND_CANCEL: u8 = 1;
const EVENT_KIND_RESIDUAL: u8 = 2;

/// `OutEvent.side` byte values (see `state::OutEvent`).
const SIDE_BID: u8 = 0;
const SIDE_ASK: u8 = 1;

/// Number of event-queue entries the permissionless `crank` drains per call.
const CRANK_BATCH_LEN: u64 = 8;

/// Convert a pure [`orderbook::Side`] into its on-chain `u8` encoding.
fn side_to_u8(side: orderbook::Side) -> u8 {
    match side {
        orderbook::Side::Bid => SIDE_BID,
        orderbook::Side::Ask => SIDE_ASK,
    }
}

/// Parse the on-chain `u8` side encoding, rejecting any value other than 0/1.
fn side_from_u8(side: u8) -> Result<orderbook::Side> {
    match side {
        SIDE_BID => Ok(orderbook::Side::Bid),
        SIDE_ASK => Ok(orderbook::Side::Ask),
        _ => Err(ProgramError::InvalidInstructionData.into()),
    }
}

/// The opposite side of `side` (every fill's maker rests opposite the taker).
fn opposite(side: orderbook::Side) -> orderbook::Side {
    match side {
        orderbook::Side::Bid => orderbook::Side::Ask,
        orderbook::Side::Ask => orderbook::Side::Bid,
    }
}

/// Load the account's active orders into the pure in-memory book.
fn load_book(account: &OrderBook) -> orderbook::Book {
    let bids = account
        .bids
        .iter()
        .filter(|o| o.active != 0)
        .map(|o| orderbook::Order {
            owner: o.owner,
            side: orderbook::Side::Bid,
            price: o.price,
            size: o.size,
            seq: o.seq,
        })
        .collect();
    let asks = account
        .asks
        .iter()
        .filter(|o| o.active != 0)
        .map(|o| orderbook::Order {
            owner: o.owner,
            side: orderbook::Side::Ask,
            price: o.price,
            size: o.size,
            seq: o.seq,
        })
        .collect();
    orderbook::Book {
        bids,
        asks,
        next_seq: account.next_seq,
    }
}

/// Write the pure book back into the account's fixed-capacity arrays, recomputing
/// the cached `best_bid`/`best_ask` from the surviving orders.
///
/// The engine never lets a side exceed `MAX_ORDERS_PER_SIDE` (`post_limit`
/// rejects at capacity and matching only removes), so contiguous indexing is
/// safe here.
fn save_book(account: &mut OrderBook, book: &orderbook::Book) {
    for slot in account.bids.iter_mut() {
        *slot = state::Order::default();
    }
    for slot in account.asks.iter_mut() {
        *slot = state::Order::default();
    }
    for (i, o) in book.bids.iter().enumerate() {
        account.bids[i] = state::Order {
            active: 1,
            owner: o.owner,
            price: o.price,
            size: o.size,
            seq: o.seq,
            _pad: [0u8; 7],
        };
    }
    for (i, o) in book.asks.iter().enumerate() {
        account.asks[i] = state::Order {
            active: 1,
            owner: o.owner,
            price: o.price,
            size: o.size,
            seq: o.seq,
            _pad: [0u8; 7],
        };
    }
    account.next_seq = book.next_seq;
    account.best_bid = orderbook::best_bid(book);
    account.best_ask = orderbook::best_ask(book);
}

/// Take the next monotonic order sequence id, incrementing the book's counter.
fn take_next_seq(book: &mut orderbook::Book) -> Result<u64> {
    let seq = book.next_seq;
    book.next_seq = book
        .next_seq
        .checked_add(1)
        .ok_or(FructusError::ArithmeticOverflow)?;
    Ok(seq)
}

/// Append an event to the bounded ring, assigning it the monotonic write-cursor
/// value as its sequence number.
///
/// Backpressure (FR-8(f)/FR-9): the ring must never overwrite an event the
/// permissionless `crank` has not yet drained. When it is full the incoming
/// event is dropped rather than silently clobbering an undrained
/// Fill/Cancel/Residual.
fn append_event(
    account: &mut OrderBook,
    kind: u8,
    owner: Pubkey,
    counterparty: Pubkey,
    side: u8,
    price: u64,
    size: u64,
) -> bool {
    let queued = account
        .event_write_cursor
        .saturating_sub(account.event_read_cursor);
    if queued >= EVENT_QUEUE_LEN as u64 {
        return false;
    }
    let idx = (account.event_write_cursor % EVENT_QUEUE_LEN as u64) as usize;
    account.events[idx] = OutEvent {
        seq: account.event_write_cursor,
        kind,
        owner,
        counterparty,
        side,
        price,
        size,
        _pad: [0u8; 6],
    };
    account.event_write_cursor = account.event_write_cursor.wrapping_add(1);
    true
}

/// Append a `Fill` event for every fill in `fills`.
///
/// Each maker's owner is carried directly on the fill (`Fill::maker_owner`),
/// set by the engine at fill time, so no pre-match snapshot is needed.
fn emit_fill_events(
    account: &mut OrderBook,
    fills: &[orderbook::Fill],
    maker_side: orderbook::Side,
) {
    for f in fills {
        append_event(
            account,
            EVENT_KIND_FILL,
            f.maker_owner,
            f.taker_owner,
            side_to_u8(maker_side),
            f.price,
            f.size,
        );
    }
}

/// Most recent TWAP sample, or `None` when nothing has been recorded yet.
///
/// A zero-initialized (never-written) slot has `slot == 0`; real samples are
/// recorded at post-genesis slots (≥ 1), so filtering on `slot != 0` cleanly
/// distinguishes the two.
fn last_observation(account: &OrderBook) -> Option<(u64, u64, u128)> {
    account
        .observations
        .iter()
        .filter(|o| o.slot != 0)
        .max_by_key(|o| o.slot)
        .map(|o| (o.slot, o.mid, u128::from_le_bytes(o.cumulative_mid)))
}

/// Record a time-weighted-mid observation: `cumulative_mid += mid * Δslots`.
///
/// Called once per book mutation. When `mid` is `None` (one-sided/empty book) the
/// contribution is `0`, so an undefined mid never pollutes the accumulator. All
/// arithmetic is `u128` + saturating — no panicking math.
fn record_observation(account: &mut OrderBook, mid: Option<u64>, now_slot: u64) {
    let idx = (account.twap_cursor % TWAP_OBSERVATIONS as u64) as usize;
    let (slot, cumulative) = match last_observation(account) {
        None => (now_slot, 0u128),
        Some((prev_slot, prev_mid, prev_cum)) => {
            let delta = now_slot.saturating_sub(prev_slot);
            // The elapsed interval `[prev_slot, now_slot)` saw the PREVIOUS
            // mid, not the post-mutation mid passed by the caller (F1): the
            // caller records the *new* mid after `save_book`, so charging it
            // to the preceding interval would bias the TWAP. A `None`
            // (one-sided book) still contributes nothing.
            let contribution = match mid {
                Some(_) => (prev_mid as u128).saturating_mul(delta as u128),
                None => 0,
            };
            (now_slot, prev_cum.saturating_add(contribution))
        }
    };
    account.observations[idx] = state::Observation {
        slot,
        mid: mid.unwrap_or(0),
        cumulative_mid: cumulative.to_le_bytes(),
    };
    account.twap_cursor = account.twap_cursor.wrapping_add(1);
}

/// Match a crossing limit taker inline and settle its remainder.
///
/// * Every fill appends a `Fill` event (maker owner carried on the fill).
/// * A budget-interrupted remainder (`MAX_MATCH_STEPS` fills reached with a
///   crossable maker still available) is re-queued as a `Residual` event.
/// * An unfilled, no-longer-crossing remainder rests at the limit price (and is
///   cancelled instead of failing when the side is at capacity).
/// * A remainder that *still* crosses after matching can only be because the
///   crossing maker is self-owned (the engine skips self-trades): it is rejected
///   with [`FructusError::SelfTrade`] when nothing filled, and cancelled (so the
///   non-self fills survive) otherwise.
fn match_limit_taker(
    account: &mut OrderBook,
    book: &mut orderbook::Book,
    incoming: orderbook::Order,
) -> Result<()> {
    let maker_side = opposite(incoming.side);
    let outcome = orderbook::match_order(
        book,
        incoming.clone(),
        orderbook::OrderKind::Limit,
        MAX_MATCH_STEPS,
    );

    emit_fill_events(account, &outcome.fills, maker_side);

    let total_filled: u64 = outcome.fills.iter().map(|f| f.size).sum();
    let remaining = incoming.size.saturating_sub(total_filled);

    match outcome.residual {
        Some(residual) => {
            // Compute budget exhausted with a crossable maker still available:
            // defer the remainder for the crank (D7), never rest a crossing order.
            let appended = append_event(
                account,
                EVENT_KIND_RESIDUAL,
                residual.owner,
                Pubkey::default(),
                side_to_u8(residual.side),
                residual.price,
                residual.size,
            );
            // Backpressure (FR-8(f)/FR-9): the ring is full, so the deferred
            // residual cannot be persisted. Fail the transaction rather than
            // silently losing the taker's still-crossable remainder (F3).
            require!(appended, FructusError::BookFull);
        }
        None if remaining > 0 => {
            // All crossable makers are gone: the remainder is now non-crossing and
            // rests, unless the book is at capacity. If it still crosses, the
            // only remaining crossable maker is self-owned.
            let remainder = orderbook::Order {
                owner: incoming.owner,
                side: incoming.side,
                price: incoming.price,
                size: remaining,
                seq: incoming.seq,
            };
            if orderbook::would_cross(book, &remainder) {
                // Reject a pure self-trade (nothing filled) with `SelfTrade`;
                // otherwise cancel the self-crossing remainder so the legitimate
                // non-self fills survive instead of reverting the taker (F4).
                if total_filled == 0 {
                    return Err(FructusError::SelfTrade.into());
                }
            } else {
                // The remainder no longer crosses and must rest at its limit
                // price. `post_limit` can only fail here with `BookFull` (the
                // price is non-zero and the remainder is non-crossing): a
                // resumed residual whose remainder cannot rest must be consumed
                // rather than reverting the whole transaction (F2), so cancel
                // the remainder instead of failing.
                let side_full = match remainder.side {
                    orderbook::Side::Bid => book.bids.len() >= MAX_ORDERS_PER_SIDE,
                    orderbook::Side::Ask => book.asks.len() >= MAX_ORDERS_PER_SIDE,
                };
                if !side_full {
                    orderbook::post_limit(book, remainder)?;
                }
            }
        }
        None => {}
    }
    Ok(())
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

    /// Create the `OrderBook` PDA bound to the market (authority-gated).
    pub fn initialize_order_book<'info>(
        ctx: Context<'info, InitializeOrderBook<'info>>,
    ) -> Result<()> {
        // `init` creates + rents the account; `load_init` gives zero-copy access
        // (the 8-byte discriminator is written by Anchor's `AccountsExit` after the
        // handler returns). A second init fails with the system "account in use".
        let order_book = &mut ctx.accounts.order_book.load_init()?;
        order_book.market = ctx.accounts.market.key();
        order_book.bump = ctx.bumps.order_book;
        order_book.next_seq = 0;
        order_book.best_bid = 0;
        order_book.best_ask = 0;
        order_book.event_read_cursor = 0;
        order_book.event_write_cursor = 0;
        order_book.twap_cursor = 0;
        // bids/asks/events/observations are zero-initialized by `load_init`.
        Ok(())
    }

    /// Post a limit order: rest if non-crossing, otherwise match inline.
    pub fn place_limit_order<'info>(
        ctx: Context<'info, PlaceLimitOrder<'info>>,
        side: u8,
        price: u64,
        size: u64,
    ) -> Result<()> {
        require!(price != 0, FructusError::InvalidPrice);
        require!(size != 0, FructusError::InvalidSize);
        let side = side_from_u8(side)?;
        let owner = ctx.accounts.owner.key();
        let mut account = ctx.accounts.order_book.load_mut()?;
        let now_slot = Clock::get()?.slot;

        let mut book = load_book(&account);
        let seq = take_next_seq(&mut book)?;
        let incoming = orderbook::Order {
            owner,
            side,
            price,
            size,
            seq,
        };

        if !orderbook::would_cross(&book, &incoming) {
            // Non-crossing: rest at the limit price.
            orderbook::post_limit(&mut book, incoming)?;
            save_book(&mut account, &book);
            record_observation(&mut account, orderbook::mid(&book), now_slot);
            return Ok(());
        }

        // Crossing: match inline; never rest a crossing order.
        match_limit_taker(&mut account, &mut book, incoming)?;
        save_book(&mut account, &book);
        record_observation(&mut account, orderbook::mid(&book), now_slot);
        Ok(())
    }

    /// Cross the opposite book best-price-first. Market orders are IOC: any
    /// unfilled remainder is cancelled, never posted.
    pub fn place_market_order<'info>(
        ctx: Context<'info, PlaceMarketOrder<'info>>,
        side: u8,
        size: u64,
    ) -> Result<()> {
        require!(size != 0, FructusError::InvalidSize);
        let side = side_from_u8(side)?;
        let owner = ctx.accounts.owner.key();
        let mut account = ctx.accounts.order_book.load_mut()?;
        let now_slot = Clock::get()?.slot;

        let mut book = load_book(&account);
        let seq = take_next_seq(&mut book)?;
        let incoming = orderbook::Order {
            owner,
            side,
            price: 0,
            size,
            seq,
        };
        let maker_side = opposite(side);

        // A market order matches to exhaustion ("until filled or the opposite
        // book is exhausted"), so there is no step budget: the opposite side has
        // at most `MAX_ORDERS_PER_SIDE` makers, which bounds the loop. `Market`
        // is IOC, so `outcome.residual` is always `None` and any unfilled
        // remainder (book exhausted) is simply cancelled, never posted.
        let outcome =
            orderbook::match_order(&mut book, incoming, orderbook::OrderKind::Market, u64::MAX);
        emit_fill_events(&mut account, &outcome.fills, maker_side);

        save_book(&mut account, &book);
        record_observation(&mut account, orderbook::mid(&book), now_slot);
        Ok(())
    }

    /// Cancel one resting order (owner-only), releasing its size.
    pub fn cancel_order<'info>(ctx: Context<'info, CancelOrder<'info>>, seq: u64) -> Result<()> {
        let owner = ctx.accounts.owner.key();
        let mut account = ctx.accounts.order_book.load_mut()?;
        let now_slot = Clock::get()?.slot;

        let mut book = load_book(&account);
        let removed = orderbook::cancel(&mut book, owner, seq)?;

        append_event(
            &mut account,
            EVENT_KIND_CANCEL,
            owner,
            Pubkey::default(),
            side_to_u8(removed.side),
            removed.price,
            removed.size,
        );
        save_book(&mut account, &book);
        record_observation(&mut account, orderbook::mid(&book), now_slot);
        Ok(())
    }

    /// Permissionless: drain the event queue in bounded batches and resume any
    /// `Residual` entries left behind by budget-interrupted takers. Never matches
    /// off-chain and takes no privileged state.
    pub fn crank<'info>(ctx: Context<'info, Crank<'info>>) -> Result<()> {
        let mut account = ctx.accounts.order_book.load_mut()?;
        let now_slot = Clock::get()?.slot;

        let mut book = load_book(&account);
        let mut book_dirty = false;

        let mut processed: u64 = 0;
        while processed < CRANK_BATCH_LEN && account.event_read_cursor < account.event_write_cursor
        {
            let idx = (account.event_read_cursor % EVENT_QUEUE_LEN as u64) as usize;
            let event = account.events[idx];
            account.event_read_cursor = account
                .event_read_cursor
                .checked_add(1)
                .ok_or(FructusError::ArithmeticOverflow)?;
            processed = processed
                .checked_add(1)
                .ok_or(FructusError::ArithmeticOverflow)?;

            match event.kind {
                EVENT_KIND_RESIDUAL => {
                    let side = side_from_u8(event.side)?;
                    let seq = take_next_seq(&mut book)?;
                    let incoming = orderbook::Order {
                        owner: event.owner,
                        side,
                        price: event.price,
                        size: event.size,
                        seq,
                    };
                    // `take_next_seq` already advanced, so the book is dirty
                    // either way. A resumed residual must be consumed, never
                    // rejected (F2): if the engine cannot finish it (a pure
                    // self-trade, or a full event ring), cancel it here so the
                    // shared crank is never wedged. The read cursor already
                    // advanced past this event.
                    book_dirty = true;
                    if match_limit_taker(&mut account, &mut book, incoming).is_err() {
                        msg!(
                            "crank cancelled unresumable residual owner={} size={}",
                            event.owner,
                            event.size
                        );
                    }
                }
                EVENT_KIND_FILL | EVENT_KIND_CANCEL => {
                    // Emit (log) and consume; there is no settlement this iteration,
                    // so fills/cancels have no further on-chain effect.
                    msg!(
                        "crank consumed event seq={} kind={} owner={} side={} price={} size={}",
                        event.seq,
                        event.kind,
                        event.owner,
                        event.side,
                        event.price,
                        event.size
                    );
                }
                _ => {
                    msg!("crank skipped unknown event kind={}", event.kind);
                }
            }
        }

        if book_dirty {
            save_book(&mut account, &book);
            record_observation(&mut account, orderbook::mid(&book), now_slot);
        }
        Ok(())
    }

    /// Create the USDC collateral-vault token account at the `PerpMarket.vault`
    /// PDA (seed `[b"vault"]`, unchanged) with the vault itself as its token
    /// authority. One-time and authority-gated; a second attempt fails with
    /// [`FructusError::VaultAlreadyInitialized`].
    pub fn initialize_collateral_vault(ctx: Context<InitializeCollateralVault>) -> Result<()> {
        // A second attempt: the vault already holds token-account data.
        require!(
            ctx.accounts.vault.data_is_empty(),
            FructusError::VaultAlreadyInitialized
        );
        // The collateral mint must be a Token-program-owned mint with 6 decimals.
        require!(
            ctx.accounts.collateral_mint.decimals == USDC_DECIMALS,
            FructusError::InvalidMint
        );

        let rent = Rent::get()?;
        let space = TokenAccount::LEN as u64;
        let bump = ctx.bumps.vault;
        let seeds: &[&[u8]] = &[VAULT_SEED, &[bump]];

        // 1. System-create the vault token account at the vault PDA.
        system_program::create_account(
            CpiContext::new(
                ctx.accounts.system_program.key(),
                system_program::CreateAccount {
                    from: ctx.accounts.payer.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                },
            )
            .with_signer(&[seeds]),
            rent.minimum_balance(TokenAccount::LEN),
            space,
            &Token::id(),
        )?;

        // 2. Initialize it as a token account whose authority is itself.
        token::initialize_account3(
            CpiContext::new(
                ctx.accounts.token_program.key(),
                token::InitializeAccount3 {
                    account: ctx.accounts.vault.to_account_info(),
                    mint: ctx.accounts.collateral_mint.to_account_info(),
                    authority: ctx.accounts.vault.to_account_info(),
                },
            )
            .with_signer(&[seeds]),
        )?;

        Ok(())
    }

    /// Deposit `amount` USDC from the user's ATA into the vault and credit the
    /// user's `UserCollateral` ledger (lazily created on first deposit).
    pub fn deposit_collateral<'info>(
        ctx: Context<'info, DepositCollateral<'info>>,
        amount: u64,
    ) -> Result<()> {
        require!(amount != 0, FructusError::InvalidSize);
        require!(
            !ctx.accounts.vault.data_is_empty(),
            FructusError::VaultNotInitialized
        );

        // Lazily create the per-(market, user) ledger on first deposit.
        if ctx.accounts.user_collateral.data_is_empty() {
            let rent = Rent::get()?;
            let space = 8 + UserCollateral::LEN;
            let market_key = ctx.accounts.market.key();
            let user_key = ctx.accounts.user.key();
            let bump = ctx.bumps.user_collateral;
            let seeds: &[&[u8]] = &[
                USER_COLLATERAL_SEED,
                market_key.as_ref(),
                user_key.as_ref(),
                &[bump],
            ];
            system_program::create_account(
                CpiContext::new(
                    ctx.accounts.system_program.key(),
                    system_program::CreateAccount {
                        from: ctx.accounts.user.to_account_info(),
                        to: ctx.accounts.user_collateral.to_account_info(),
                    },
                )
                .with_signer(&[seeds]),
                rent.minimum_balance(space),
                space as u64,
                &crate::ID,
            )?;
        }

        // Move `amount` USDC from the user's ATA into the vault (user signs).
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.key(),
                token::Transfer {
                    from: ctx.accounts.user_ata.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                    authority: ctx.accounts.user.to_account_info(),
                },
            ),
            amount,
        )?;

        // Credit the ledger with checked arithmetic (atomic: any error unwinds).
        let mut user_collateral =
            Account::<UserCollateral>::try_from_unchecked(ctx.accounts.user_collateral.as_ref())?;
        user_collateral.bump = ctx.bumps.user_collateral;
        user_collateral.deposited = collateral::deposit(user_collateral.deposited, amount)
            .ok_or(FructusError::ArithmeticOverflow)?;
        user_collateral.exit(&crate::ID)?;

        Ok(())
    }

    /// Withdraw `amount` USDC from the vault to the user's ATA, gated by the
    /// free-collateral seam (`amount <= deposited - reserved`).
    pub fn withdraw_collateral(ctx: Context<WithdrawCollateral>, amount: u64) -> Result<()> {
        require!(amount != 0, FructusError::InvalidSize);
        require!(
            !ctx.accounts.vault.data_is_empty(),
            FructusError::VaultNotInitialized
        );

        let user_collateral = &mut ctx.accounts.user_collateral;

        // Enforce the free-collateral seam (reserved is always 0 this iteration,
        // so this reduces to `amount <= deposited`), computing the post-withdraw
        // balance up front so the ledger is debited only after the transfer.
        let new_deposited =
            collateral::withdraw(user_collateral.deposited, user_collateral.reserved, amount)
                .ok_or(FructusError::InsufficientFreeCollateral)?;

        // Move `amount` USDC from the vault to the user's ATA (vault PDA signs).
        let bump = ctx.bumps.vault;
        let seeds: &[&[u8]] = &[VAULT_SEED, &[bump]];
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.key(),
                token::Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.user_ata.to_account_info(),
                    authority: ctx.accounts.vault.to_account_info(),
                },
            )
            .with_signer(&[seeds]),
            amount,
        )?;

        user_collateral.deposited = new_deposited;

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

#[derive(Accounts)]
pub struct InitializeOrderBook<'info> {
    #[account(
        init,
        payer = payer,
        space = 8 + OrderBook::LEN,
        seeds = [ORDER_BOOK_SEED, market.key().as_ref()],
        bump
    )]
    pub order_book: AccountLoader<'info, OrderBook>,
    #[account(
        seeds = [PERP_MARKET_SEED],
        bump = market.bump,
        has_one = authority
    )]
    pub market: Account<'info, PerpMarket>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PlaceLimitOrder<'info> {
    #[account(
        mut,
        seeds = [ORDER_BOOK_SEED, market.key().as_ref()],
        bump
    )]
    pub order_book: AccountLoader<'info, OrderBook>,
    #[account(seeds = [PERP_MARKET_SEED], bump = market.bump)]
    pub market: Account<'info, PerpMarket>,
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct PlaceMarketOrder<'info> {
    #[account(
        mut,
        seeds = [ORDER_BOOK_SEED, market.key().as_ref()],
        bump
    )]
    pub order_book: AccountLoader<'info, OrderBook>,
    #[account(seeds = [PERP_MARKET_SEED], bump = market.bump)]
    pub market: Account<'info, PerpMarket>,
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct CancelOrder<'info> {
    #[account(
        mut,
        seeds = [ORDER_BOOK_SEED, market.key().as_ref()],
        bump
    )]
    pub order_book: AccountLoader<'info, OrderBook>,
    #[account(seeds = [PERP_MARKET_SEED], bump = market.bump)]
    pub market: Account<'info, PerpMarket>,
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct Crank<'info> {
    #[account(
        mut,
        seeds = [ORDER_BOOK_SEED, market.key().as_ref()],
        bump
    )]
    pub order_book: AccountLoader<'info, OrderBook>,
    #[account(seeds = [PERP_MARKET_SEED], bump = market.bump)]
    pub market: Account<'info, PerpMarket>,
    pub cranker: Signer<'info>,
}

#[derive(Accounts)]
pub struct InitializeCollateralVault<'info> {
    #[account(
        seeds = [PERP_MARKET_SEED],
        bump = market.bump,
        has_one = authority
    )]
    pub market: Account<'info, PerpMarket>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: the vault token-account PDA, system-created and initialized by
    /// CPI in the handler (authority = the vault itself).
    #[account(mut, seeds = [VAULT_SEED], bump)]
    pub vault: UncheckedAccount<'info>,
    #[account(address = market.collateral_mint)]
    pub collateral_mint: Account<'info, Mint>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct DepositCollateral<'info> {
    #[account(mut)]
    pub user: Signer<'info>,
    #[account(seeds = [PERP_MARKET_SEED], bump = market.bump)]
    pub market: Account<'info, PerpMarket>,
    /// CHECK: the per-(market, user) ledger, lazily created by the handler on
    /// first deposit.
    #[account(
        mut,
        seeds = [USER_COLLATERAL_SEED, market.key().as_ref(), user.key().as_ref()],
        bump
    )]
    pub user_collateral: UncheckedAccount<'info>,
    /// CHECK: the vault token account (authority = the vault PDA); the handler
    /// rejects an uninitialized vault with `VaultNotInitialized` before use.
    #[account(mut, seeds = [VAULT_SEED], bump)]
    pub vault: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = collateral_mint,
        associated_token::authority = user
    )]
    pub user_ata: Account<'info, TokenAccount>,
    #[account(address = market.collateral_mint)]
    pub collateral_mint: Account<'info, Mint>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct WithdrawCollateral<'info> {
    pub user: Signer<'info>,
    #[account(seeds = [PERP_MARKET_SEED], bump = market.bump)]
    pub market: Account<'info, PerpMarket>,
    #[account(
        mut,
        seeds = [USER_COLLATERAL_SEED, market.key().as_ref(), user.key().as_ref()],
        bump
    )]
    pub user_collateral: Account<'info, UserCollateral>,
    /// CHECK: the vault token account (authority = the vault PDA); the handler
    /// rejects an uninitialized vault with `VaultNotInitialized` before use.
    #[account(mut, seeds = [VAULT_SEED], bump)]
    pub vault: UncheckedAccount<'info>,
    #[account(
        mut,
        associated_token::mint = collateral_mint,
        associated_token::authority = user
    )]
    pub user_ata: Account<'info, TokenAccount>,
    #[account(address = market.collateral_mint)]
    pub collateral_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod handlers_tests {
    use super::*;

    fn empty_account() -> OrderBook {
        OrderBook {
            market: Pubkey::default(),
            bump: 0,
            next_seq: 0,
            best_bid: 0,
            best_ask: 0,
            event_read_cursor: 0,
            event_write_cursor: 0,
            twap_cursor: 0,
            _pad: [0u8; 7],
            bids: [state::Order::default(); crate::constants::MAX_ORDERS_PER_SIDE],
            asks: [state::Order::default(); crate::constants::MAX_ORDERS_PER_SIDE],
            events: [OutEvent::default(); EVENT_QUEUE_LEN],
            observations: [state::Observation::default(); TWAP_OBSERVATIONS],
        }
    }

    fn order(
        owner: u8,
        side: orderbook::Side,
        price: u64,
        size: u64,
        seq: u64,
    ) -> orderbook::Order {
        orderbook::Order {
            owner: Pubkey::from([owner; 32]),
            side,
            price,
            size,
            seq,
        }
    }

    #[test]
    fn side_encoding_round_trips() {
        assert_eq!(side_to_u8(orderbook::Side::Bid), SIDE_BID);
        assert_eq!(side_to_u8(orderbook::Side::Ask), SIDE_ASK);
        assert_eq!(side_from_u8(SIDE_BID).unwrap(), orderbook::Side::Bid);
        assert_eq!(side_from_u8(SIDE_ASK).unwrap(), orderbook::Side::Ask);
        assert!(side_from_u8(2).is_err());
        assert!(side_from_u8(255).is_err());
    }

    #[test]
    fn load_save_round_trips_orders() {
        let mut account = empty_account();
        account.bids[0] = state::Order {
            active: 1,
            owner: Pubkey::from([1; 32]),
            price: 9,
            size: 5,
            seq: 0,
            _pad: [0u8; 7],
        };
        account.bids[1] = state::Order {
            active: 1,
            owner: Pubkey::from([2; 32]),
            price: 10,
            size: 7,
            seq: 1,
            _pad: [0u8; 7],
        };
        account.asks[0] = state::Order {
            active: 1,
            owner: Pubkey::from([3; 32]),
            price: 11,
            size: 3,
            seq: 2,
            _pad: [0u8; 7],
        };
        account.next_seq = 3;

        let book = load_book(&account);
        assert_eq!(book.bids.len(), 2);
        assert_eq!(book.asks.len(), 1);
        assert_eq!(orderbook::best_bid(&book), 10);
        assert_eq!(orderbook::best_ask(&book), 11);

        let mut out = empty_account();
        save_book(&mut out, &book);
        assert_eq!(out.best_bid, 10);
        assert_eq!(out.best_ask, 11);
        assert_eq!(out.next_seq, 3);
        assert!(out.bids[0].active == 1 && out.bids[0].seq == 0);
        assert!(out.bids[1].active == 1 && out.bids[1].seq == 1);
        assert!(out.bids[2].active == 0);
        assert!(out.asks[0].active == 1);
        assert!(out.asks[1].active == 0);
    }

    #[test]
    fn append_event_is_monotonic_and_backpressures_at_capacity() {
        let mut account = empty_account();
        for i in 0..(EVENT_QUEUE_LEN as u64) {
            append_event(
                &mut account,
                EVENT_KIND_FILL,
                Pubkey::from([7; 32]),
                Pubkey::default(),
                SIDE_BID,
                i,
                i,
            );
        }
        // The ring is exactly full: every event was written, seq == write cursor.
        assert_eq!(account.event_write_cursor, EVENT_QUEUE_LEN as u64);
        let last_idx = (EVENT_QUEUE_LEN as u64 - 1) as usize;
        assert_eq!(account.events[last_idx].seq, EVENT_QUEUE_LEN as u64 - 1);

        // A wrapping write with an undrained ring must not overwrite: the oldest
        // event (seq 0) stays in slot 0 and the write cursor stops advancing.
        append_event(
            &mut account,
            EVENT_KIND_FILL,
            Pubkey::from([7; 32]),
            Pubkey::default(),
            SIDE_BID,
            999,
            999,
        );
        assert_eq!(
            account.event_write_cursor, EVENT_QUEUE_LEN as u64,
            "backpressure must not advance the cursor past an undrained ring"
        );
        assert_eq!(
            account.events[0].seq, 0,
            "undrained slot must not be overwritten"
        );

        // Draining one slot frees capacity for the next append.
        account.event_read_cursor = 1;
        append_event(
            &mut account,
            EVENT_KIND_FILL,
            Pubkey::from([7; 32]),
            Pubkey::default(),
            SIDE_BID,
            999,
            999,
        );
        assert_eq!(account.event_write_cursor, EVENT_QUEUE_LEN as u64 + 1);
        assert_eq!(
            account.events[0].seq, EVENT_QUEUE_LEN as u64,
            "drained slot reused"
        );
    }

    #[test]
    fn record_observation_accumulates_mid_times_delta() {
        let mut account = empty_account();
        // First sample: no prior sample, cumulative stays 0.
        record_observation(&mut account, Some(7), 100);
        assert_eq!(account.observations[0].slot, 100);
        assert_eq!(
            u128::from_le_bytes(account.observations[0].cumulative_mid),
            0
        );

        // 10 slots later with mid 7: cumulative += 7 * 10.
        record_observation(&mut account, Some(7), 110);
        assert_eq!(account.observations[1].slot, 110);
        assert_eq!(
            u128::from_le_bytes(account.observations[1].cumulative_mid),
            70
        );

        // A one-sided book (mid None) contributes nothing but still records a slot.
        record_observation(&mut account, None, 120);
        assert_eq!(account.observations[2].slot, 120);
        assert_eq!(
            u128::from_le_bytes(account.observations[2].cumulative_mid),
            70
        );
    }

    #[test]
    fn match_limit_taker_rejects_self_trade() {
        let mut account = empty_account();
        let mut book = orderbook::Book {
            bids: vec![],
            asks: vec![order(1, orderbook::Side::Ask, 10, 5, 0)],
            next_seq: 1,
        };
        // Taker (owner 1) bids 12 into its own ask at 10: skipped by the engine,
        // then the remainder still crosses -> SelfTrade.
        let incoming = order(1, orderbook::Side::Bid, 12, 5, 1);
        let result = match_limit_taker(&mut account, &mut book, incoming);
        assert!(result.is_err());
    }

    #[test]
    fn match_limit_taker_rests_non_crossing_remainder() {
        let mut account = empty_account();
        let mut book = orderbook::Book {
            bids: vec![],
            asks: vec![order(2, orderbook::Side::Ask, 10, 3, 0)],
            next_seq: 1,
        };
        // Taker bids 12 for size 5 into ask 10 (size 3): fills 3, rests 2 at 12.
        let incoming = order(1, orderbook::Side::Bid, 12, 5, 1);
        match_limit_taker(&mut account, &mut book, incoming).unwrap();
        assert!(book.asks.is_empty());
        assert_eq!(book.bids.len(), 1);
        assert_eq!(book.bids[0].price, 12);
        assert_eq!(book.bids[0].size, 2);
    }

    #[test]
    fn match_limit_taker_requeues_on_budget_hit() {
        let mut account = empty_account();
        let asks: Vec<orderbook::Order> = (0..10)
            .map(|i| order(2, orderbook::Side::Ask, 10 + i as u64, 1, i as u64))
            .collect();
        let mut book = orderbook::Book {
            bids: vec![],
            asks,
            next_seq: 10,
        };
        // MAX_MATCH_STEPS == 8, so 8 of the 10 makers fill and the remaining 2
        // are deferred as a Residual event.
        let incoming = order(1, orderbook::Side::Bid, 30, 10, 0);
        match_limit_taker(&mut account, &mut book, incoming).unwrap();
        assert_eq!(book.asks.len(), 2);
        assert_eq!(account.event_write_cursor, 9); // 8 fills + 1 residual
        assert_eq!(account.events[8].kind, EVENT_KIND_RESIDUAL);
        assert_eq!(account.events[8].size, 2);
    }
}
