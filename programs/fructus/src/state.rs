//! Yield oracle account state and its pure, property-testable helpers.

use anchor_lang::prelude::*;
use sha2::{Digest, Sha256};

use crate::constants::{
    EVENT_QUEUE_LEN, FUNDING_K_MAX, FUNDING_K_MIN, MAX_APY, MAX_FUNDING_MAX, MAX_MARGIN_BPS,
    MAX_ORDERS_PER_SIDE, TWAP_OBSERVATIONS, UPDATE_DOMAIN_SEPARATOR,
};
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

/// Singleton perpetual-market configuration (perpetual futures over an LST index).
///
/// Stored as a singleton PDA (see [`crate::constants::PERP_MARKET_SEED`]). Created
/// once by `initialize_market`; holds the trustless jitoSOL index source, the USDC
/// collateral mint, funding/margin parameters, the admin authority, and the
/// collateral-vault PDA pubkey (the vault token account itself is created later).
#[account]
pub struct PerpMarket {
    /// jitoSOL SPL Stake Pool account used as the trustless index source.
    pub index_source: Pubkey,
    /// USDC collateral mint.
    pub collateral_mint: Pubkey,
    /// Funding convergence speed, fixed-point scaled by `APY_SCALE`.
    pub funding_k: u64,
    /// Per-epoch funding-rate cap, fixed-point scaled by `APY_SCALE`.
    pub max_funding: u64,
    /// Funding epoch length in slots.
    pub funding_epoch_slots: u64,
    /// Initial margin requirement, in basis points.
    pub initial_margin_bps: u16,
    /// Maintenance margin requirement, in basis points.
    pub maintenance_margin_bps: u16,
    /// Admin authority authorized to manage the market.
    pub authority: Pubkey,
    /// Collateral-custody vault PDA (derived from `VAULT_SEED`, not created at init).
    pub vault: Pubkey,
    /// Market PDA bump seed.
    pub bump: u8,
}

impl PerpMarket {
    /// Serialized size of the account payload (excluding the 8-byte discriminator).
    ///
    /// Packed borsh layout: `32 + 32 + 8 + 8 + 8 + 2 + 2 + 32 + 32 + 1 = 157`.
    pub const LEN: usize = 32 + 32 + 8 + 8 + 8 + 2 + 2 + 32 + 32 + 1;
}

// --- Order book + collateral vault (issues #3 & #4) ---
//
// The account-level types below are SEPARATE from the pure `crate::orderbook`
// types (which hold a `side` field and use `Vec`s): the instruction handlers in
// `lib.rs` load the `bids`/`asks` arrays into the pure in-memory book, run the
// matching engine, and convert back on save. `side` is implied by which array an
// `Order` slot sits in, so it is not stored on the account-level `Order`.

/// A single resting-order slot inside the on-chain order book.
///
/// `active` distinguishes an empty slot from a resting order, so `price == 0`
/// remains an *invalid price* (rejected with [`FructusError::InvalidPrice`])
/// rather than an ambiguity with an empty slot.
#[zero_copy]
#[derive(Debug, PartialEq, Eq)]
pub struct Order {
    /// The signer who placed the order.
    pub owner: Pubkey,
    /// Traded yield level in `APY_SCALE` fixed point; `0` is invalid for a live order.
    pub price: u64,
    /// Remaining (unfilled) size, in notional USDC microunits.
    pub size: u64,
    /// Monotonic order id giving time priority within a price level.
    pub seq: u64,
    /// Whether this slot holds a resting order (`0` = empty slot). `u8` because
    /// `bytemuck::Pod` forbids `bool` (which has invalid bit patterns).
    pub active: u8,
    /// Explicit padding so the `#[repr(C)]` zero-copy layout is packing-free
    /// (required by `bytemuck::Pod`).
    pub _pad: [u8; 7],
}

impl Default for Order {
    fn default() -> Self {
        Self {
            owner: Pubkey::default(),
            price: 0,
            size: 0,
            seq: 0,
            active: 0,
            _pad: [0u8; 7],
        }
    }
}

impl Order {
    /// In-memory `#[repr(C)]` size (`64` bytes, incl. the explicit padding).
    pub const LEN: usize = std::mem::size_of::<Self>();
}

/// One outcome recorded on the bounded event-queue ring.
#[zero_copy]
#[derive(Debug, PartialEq, Eq)]
pub struct OutEvent {
    /// Monotonic event sequence number.
    pub seq: u64,
    /// The traded price (fill) or the order's price (cancel/residual).
    pub price: u64,
    /// The traded size (fill) or remaining size (cancel/residual).
    pub size: u64,
    /// The order's owner.
    pub owner: Pubkey,
    /// The counterparty that matched this order (zero pubkey when unset).
    pub counterparty: Pubkey,
    /// Fill-time index snapshot numerator: pool `total_lamports` when the fill
    /// executed (design D7/D8). `0` on non-fill events (Cancel/Residual).
    pub entry_total_lamports: u64,
    /// Fill-time index snapshot denominator: pool token supply when the fill
    /// executed (design D7/D8). `0` on non-fill events (Cancel/Residual).
    pub entry_pool_token_supply: u64,
    /// Whether a maker settlement has consumed this Fill (`0` = pending; the
    /// `settle_fill` instruction flips it to `1`). Meaningless on other kinds.
    pub settled: u8,
    /// Event kind: `0` = Fill, `1` = Cancel, `2` = Residual.
    pub kind: u8,
    /// Which side the order was on: `0` = Bid, `1` = Ask.
    pub side: u8,
    /// Explicit padding so the `#[repr(C)]` zero-copy layout is packing-free.
    pub _pad: [u8; 5],
}

impl Default for OutEvent {
    fn default() -> Self {
        Self {
            seq: 0,
            price: 0,
            size: 0,
            owner: Pubkey::default(),
            counterparty: Pubkey::default(),
            entry_total_lamports: 0,
            entry_pool_token_supply: 0,
            settled: 0,
            kind: 0,
            side: 0,
            _pad: [0u8; 5],
        }
    }
}

impl OutEvent {
    /// In-memory `#[repr(C)]` size (`112` bytes, incl. the explicit padding).
    pub const LEN: usize = std::mem::size_of::<Self>();
}

/// One time-weighted-mid accumulator sample on the TWAP ring.
#[zero_copy]
#[derive(Debug, PartialEq, Eq)]
pub struct Observation {
    /// Slot at which this sample was recorded.
    pub slot: u64,
    /// Mid price in effect as of this sample (`0` = book one-sided/undefined).
    pub mid: u64,
    /// Running `Σ mid × Δslot` accumulator, stored as 16 raw bytes (`u128` is
    /// avoided for cross-target alignment stability in a zero-copy layout).
    pub cumulative_mid: [u8; 16],
}

impl Default for Observation {
    fn default() -> Self {
        Self {
            slot: 0,
            mid: 0,
            cumulative_mid: [0u8; 16],
        }
    }
}

impl Observation {
    /// In-memory `#[repr(C)]` size (`32` bytes, no padding).
    pub const LEN: usize = std::mem::size_of::<Self>();
}

/// On-chain order book: one PDA per market, holding the full bid/ask book, the
/// event queue, and the TWAP accumulator inline (no per-order PDAs, no off-chain
/// state).
///
/// Seed `[ORDER_BOOK_SEED, market.key()]` binds each book to exactly one market.
#[account(zero_copy)]
#[derive(Debug)]
pub struct OrderBook {
    /// Monotonic order id; incremented on every accepted order.
    pub next_seq: u64,
    /// Highest resting bid price (`0` = bid side empty).
    pub best_bid: u64,
    /// Lowest resting ask price (`0` = ask side empty).
    pub best_ask: u64,
    /// Event-queue read cursor (index of the next event to drain).
    pub event_read_cursor: u64,
    /// Event-queue write cursor (index of the next event slot to write).
    pub event_write_cursor: u64,
    /// TWAP ring cursor (index of the next observation slot).
    pub twap_cursor: u64,
    /// The market this book is bound to (also present in the PDA seed).
    pub market: Pubkey,
    /// PDA bump seed.
    pub bump: u8,
    /// Explicit padding so the header is 8-aligned (required by `bytemuck::Pod`).
    pub _pad: [u8; 7],
    /// Resting bids (side implied by the array).
    pub bids: [Order; MAX_ORDERS_PER_SIDE],
    /// Resting asks (side implied by the array).
    pub asks: [Order; MAX_ORDERS_PER_SIDE],
    /// Bounded ring of order outcomes (fills / cancels / residuals).
    pub events: [OutEvent; EVENT_QUEUE_LEN],
    /// TWAP ring of time-weighted-mid observations.
    pub observations: [Observation; TWAP_OBSERVATIONS],
}

impl Default for OrderBook {
    /// Build the all-zero book via `bytemuck::Zeroable::zeroed()`.
    ///
    /// The previous per-field literal constructed every fixed-capacity array
    /// (`[OutEvent::default(); EVENT_QUEUE_LEN]`, `[Order::default();
    /// MAX_ORDERS_PER_SIDE]`, …) as large stack temporaries, so the SBF-derived
    /// `OrderBook::default` frame exceeded the 4 KiB Solana stack budget and
    /// `anchor build`/`cargo build-sbf` warned "Stack offset … exceeded max
    /// offset of 4096". `#[zero_copy]` marks the struct `bytemuck::Pod` +
    /// `Zeroable`, so an all-zero value IS the canonical default and is
    /// produced in place (no big callee stack frame). A zero-copy account's
    /// default must be all zeroes.
    fn default() -> Self {
        bytemuck::Zeroable::zeroed()
    }
}

impl OrderBook {
    /// In-memory `#[repr(C)]` size of the account payload (excluding the 8-byte
    /// discriminator). Header `next_seq/best_bid/best_ask/cursors (6×8) + market
    /// (32) + bump (1) + _pad (7) = 88`, then the four fixed-capacity arrays.
    pub const LEN: usize = std::mem::size_of::<Self>();
}

/// Per-`(market, user)` collateral ledger, one PDA per user per market.
///
/// Seed `[USER_COLLATERAL_SEED, market.key(), user.key()]`. Lazily initialized on
/// first deposit (payer = user). Both amounts are USDC microunits; `reserved` is
/// stubbed to `0` this iteration (no positions yet), so free collateral equals
/// `deposited`.
#[account]
pub struct UserCollateral {
    /// USDC deposited by the user, in microunits (6 decimals).
    pub deposited: u64,
    /// USDC reserved for open positions, in microunits (always `0` this iteration).
    pub reserved: u64,
    /// PDA bump seed.
    pub bump: u8,
}

impl UserCollateral {
    /// Serialized size of the account payload (excluding the 8-byte discriminator).
    ///
    /// Packed borsh layout: `deposited(8) + reserved(8) + bump(1) = 17`.
    pub const LEN: usize = 8 + 8 + 1;
}

// --- Position lifecycle (issue #5) ---

/// Per-`(market, user, side)` position ledger, one PDA per user per market side.
///
/// Seed `[POSITION_SEED, market.key(), user.key(), side]` (see
/// [`crate::constants::POSITION_SEED`] = `b"position"`). Lazily created on
/// first fill/settlement (payer = user/cranker) and **retained** after a full
/// close: `notional == 0` means the position is closed, and a re-open resets
/// `entry_n_sum`/`entry_d_sum`/`open_slot`. Margin is ledger-only: `collateral`
/// mirrors the reserved-margin bookkeeping in [`UserCollateral::reserved`], with
/// no token movement on open or close.
///
/// `side` reuses the book-side encoding: `0` = Long/Bid, `1` = Short/Ask. It is
/// both stored and part of the seed (self-describing, like `OrderBook.market`).
#[account]
pub struct Position {
    /// The market this position trades on (also present in the PDA seed).
    pub market: Pubkey,
    /// The user holding the position (also present in the PDA seed).
    pub owner: Pubkey,
    /// Book side this position opens on: `0` = Long/Bid, `1` = Short/Ask.
    pub side: u8,
    /// Remaining position, in notional USDC microunits. `0` == closed.
    pub notional: u64,
    /// `Σ(total_lamports × fill_size)`: notional-weighted entry-index running sum.
    ///
    /// Stored as a native `u128` (borsh-serializes to 16 LE bytes); the entry
    /// index snapshot rate `entry_n_sum / entry_d_sum` is computed at PnL time
    /// after a shared power-of-two normalization — no intermediate rounding.
    pub entry_n_sum: u128,
    /// `Σ(pool_token_supply × fill_size)`: notional-weighted entry-index running sum.
    pub entry_d_sum: u128,
    /// Reserved margin for this position, in USDC microunits: always equals
    /// `margin_required(notional, initial_margin_bps)`.
    pub collateral: u64,
    /// Last funding epoch this position was settled for (stored; written from #6).
    pub last_funding_epoch: u64,
    /// Slot at which the position was (re)created: the fill slot for inline
    /// taker opens, or the settlement slot for maker re-opens via `settle_fill`.
    pub open_slot: u64,
    /// PDA bump seed.
    pub bump: u8,
}

impl Position {
    /// Serialized size of the account payload (excluding the 8-byte discriminator).
    ///
    /// Packed borsh layout: `32 + 32 + 1 + 8 + 16 + 16 + 8 + 8 + 8 + 1 = 130`.
    pub const LEN: usize = 32 + 32 + 1 + 8 + 16 + 16 + 8 + 8 + 8 + 1;
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

/// Whether a funding convergence-speed value lies within `[FUNDING_K_MIN, FUNDING_K_MAX]`.
pub fn funding_k_in_bounds(k: u64) -> bool {
    k >= FUNDING_K_MIN && k <= FUNDING_K_MAX
}

/// Whether a per-epoch funding-rate cap lies within `[0, MAX_FUNDING_MAX]`.
pub fn max_funding_in_bounds(m: u64) -> bool {
    m <= MAX_FUNDING_MAX
}

/// Whether an initial margin (basis points) lies within `(0, MAX_MARGIN_BPS]`.
pub fn initial_margin_in_bounds(im: u16) -> bool {
    im > 0 && im <= MAX_MARGIN_BPS
}

/// Whether a maintenance margin (basis points) lies within `(0, im]`.
pub fn maintenance_margin_in_bounds(im: u16, mm: u16) -> bool {
    mm > 0 && mm <= im
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

#[cfg(test)]
mod tests {
    use anchor_lang::prelude::*;

    use super::{Observation, Order, OrderBook, OutEvent, Position, UserCollateral};

    /// Every zero-copy `LEN` constant must equal the in-memory `#[repr(C)]` size
    /// of its type — the exact invariant the `space = 8 + LEN` constraints rely
    /// on (zero-copy accounts are reinterpreted in place, not borsh-serialized).
    #[test]
    fn zero_copy_len_constants_match_size_of() {
        assert_eq!(Order::LEN, std::mem::size_of::<Order>());
        assert_eq!(OutEvent::LEN, std::mem::size_of::<OutEvent>());
        assert_eq!(Observation::LEN, std::mem::size_of::<Observation>());
        assert_eq!(OrderBook::LEN, std::mem::size_of::<OrderBook>());
    }

    /// `UserCollateral` is still a borsh `#[account]`; its `LEN` must equal the
    /// packed borsh payload size (excluding the discriminator).
    #[test]
    fn user_collateral_len_matches_borsh_payload() {
        let uc = UserCollateral {
            deposited: 0,
            reserved: 0,
            bump: 0,
        };
        assert_eq!(borsh::to_vec(&uc).unwrap().len(), UserCollateral::LEN);
    }

    /// Pin the exact byte sizes so a future field/constant edit cannot silently
    /// drift the account size or layout.
    #[test]
    fn len_constants_match_documented_sizes() {
        assert_eq!(Order::LEN, 64);
        assert_eq!(OutEvent::LEN, 112);
        assert_eq!(Observation::LEN, 32);
        assert_eq!(UserCollateral::LEN, 17);
        assert_eq!(OrderBook::LEN, 23_128);
    }

    /// `Position` is a borsh `#[account]`; its `LEN` must equal the packed borsh
    /// payload size (excluding the discriminator), like `UserCollateral::LEN`.
    #[test]
    fn position_len_matches_borsh_payload() {
        let pos = Position {
            market: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            side: 1,
            notional: 1_000_000,
            entry_n_sum: 1_234_567_890,
            entry_d_sum: 9_876_543_210,
            collateral: 100_000,
            last_funding_epoch: 42,
            open_slot: 7,
            bump: 255,
        };
        assert_eq!(borsh::to_vec(&pos).unwrap().len(), Position::LEN);
        // Pin the documented size so a field/constant edit cannot drift it.
        assert_eq!(Position::LEN, 130);
    }

    /// The `Position` PDA seed `[POSITION_SEED, market, user, side]` must
    /// round-trip: `create_program_address` fed the bump that
    /// `find_program_address` returned reproduces the PDA, for both side
    /// encodings (`0` = Long/Bid, `1` = Short/Ask).
    #[test]
    fn position_pda_seed_round_trip() {
        use crate::constants::POSITION_SEED;

        let market = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        for side in [0u8, 1u8] {
            let seeds: &[&[u8]] = &[POSITION_SEED, market.as_ref(), user.as_ref(), &[side]];
            let (pda, bump) = Pubkey::find_program_address(seeds, &crate::ID);
            // `create_program_address` re-derives the PDA from the same seeds
            // plus the bump byte `find_program_address` returned; a `Position`
            // account stores exactly that `bump` to re-derive on every access.
            let bump_seed = [bump];
            let full_seeds: &[&[u8]] = &[
                POSITION_SEED,
                market.as_ref(),
                user.as_ref(),
                &[side],
                &bump_seed,
            ];
            let created = Pubkey::create_program_address(full_seeds, &crate::ID).unwrap();
            // Byte-level compare per AGENTS.md (no type-identity dependence).
            assert_eq!(pda.as_ref(), created.as_ref());
        }
    }

    /// `OrderBook::default()` must be the all-zero canonical value — identical to
    /// a zeroed `bytemuck` buffer — because `initialize_order_book` creates the
    /// account via `load_init` (zeroed memory) and reinterprets the bytes in
    /// place (zero-copy), so a default must never carry stale non-zero bytes.
    ///
    /// This pins the `Default` impl that was changed to
    /// `bytemuck::Zeroable::zeroed()`: the previous per-field literal built every
    /// fixed-capacity array (`[OutEvent::default(); EVENT_QUEUE_LEN]`, …) as large
    /// stack temporaries, so the SBF-derived `OrderBook::default` frame exceeded
    /// the 4 KiB Solana stack budget and `cargo build-sbf` / `anchor build`
    /// reported "Stack offset … exceeded max offset of 4096". The SBF build is the
    /// true guard for that stack-size issue; this host test guards the value
    /// semantics (all-zero) that the change must preserve.
    #[test]
    fn order_book_default_is_all_zero() {
        let d = OrderBook::default();
        let z: OrderBook = bytemuck::Zeroable::zeroed();
        assert_eq!(
            bytemuck::bytes_of(&d),
            bytemuck::bytes_of(&z),
            "default() must equal a zeroed OrderBook so initialize_order_book's \
             load_init semantics stay byte-identical"
        );
        // Spot-check the header and a few ring slots are zero.
        assert_eq!(d.next_seq, 0);
        assert_eq!(d.best_bid, 0);
        assert_eq!(d.best_ask, 0);
        assert_eq!(d.event_read_cursor, 0);
        assert_eq!(d.event_write_cursor, 0);
        assert_eq!(d.twap_cursor, 0);
        assert_eq!(d.market, Pubkey::default());
        assert_eq!(d.bump, 0);
        assert!(d.bids.iter().all(|o| o.active == 0));
        assert!(d.asks.iter().all(|o| o.active == 0));
        assert!(d
            .events
            .iter()
            .all(|e| e.seq == 0 && e.kind == 0 && e.settled == 0));
        assert!(d.observations.iter().all(|o| o.slot == 0));
    }
}
