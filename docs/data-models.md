# Data Models

On-chain data shapes: the oracle account, the perpetual-market account, the
order book (with its inline `Order`/`OutEvent`/`Observation` sub-structs), the
collateral ledger, the per-`(market, user, side)` position ledger, and the
derived exchange rate.

## `YieldOracle` (anchor account)

Singleton PDA, seed `"yield_oracle"`. Borsh layout (after the 8-byte discriminator):

| Field | Type | Offset (payload) | Notes |
| --- | --- | --- | --- |
| `apy` | u64 | 0 | scaled by `1_000_000` |
| `version` | u64 | 8 | monotonic |
| `last_update_slot` | u64 | 16 | |
| `publisher` | Pubkey | 24 | authorized signer |
| `authority` | Pubkey | 56 | admin |
| `stale_after_slots` | u64 | 88 | staleness window |
| `bump` | u8 | 96 | PDA bump |

## `PerpMarket` (anchor account)

Singleton PDA, seed `"perp_market"`. Borsh layout (after the 8-byte discriminator):

| Field | Type | Offset (payload) | Notes |
| --- | --- | --- | --- |
| `index_source` | Pubkey | 0 | jitoSOL SPL Stake Pool account (validated at init) |
| `collateral_mint` | Pubkey | 32 | USDC mint (stored as-is, no on-chain check) |
| `funding_k` | u64 | 64 | convergence speed; fixed-point scale `1_000_000` |
| `max_funding` | u64 | 72 | per-epoch funding-rate cap; fixed-point scale `1_000_000` |
| `funding_epoch_slots` | u64 | 80 | epoch length in slots |
| `initial_margin_bps` | u16 | 88 | initial margin, basis points |
| `maintenance_margin_bps` | u16 | 90 | maintenance margin, basis points |
| `authority` | Pubkey | 92 | admin |
| `vault` | Pubkey | 124 | collateral-custody PDA (derived, not created at init) |
| `bump` | u8 | 156 | PDA bump |

- `LEN = 157` (packed borsh payload, excluding the 8-byte discriminator).
- Singleton seed `"perp_market"`; `index_source` and `collateral_mint` are plain
  fields, not seed components.
- `funding_k` / `max_funding` use the fixed-point scale `1_000_000`
  (`1.0 == 1_000_000`); the two margin fields use basis points (≤ `10_000`).

## `OrderBook` (zero-copy account)

One PDA per market, seed `[b"order_book", market.key()]`. Zero-copy (`#[repr(C)]`)
layout (after the 8-byte discriminator); fields are reordered and explicitly
padded so `bytemuck::Pod` has no implicit padding:

| Field | Type | Offset (payload) | Notes |
| --- | --- | --- | --- |
| `next_seq` | u64 | 0 | monotonic order id |
| `best_bid` | u64 | 8 | highest resting bid; `0` = side empty |
| `best_ask` | u64 | 16 | lowest resting ask; `0` = side empty |
| `event_read_cursor` | u64 | 24 | next event to drain |
| `event_write_cursor` | u64 | 32 | next event slot to write |
| `twap_cursor` | u64 | 40 | next TWAP observation slot |
| `market` | Pubkey | 48 | bound market (also in the PDA seed) |
| `bump` | u8 | 80 | PDA bump |
| `_pad` | `[u8; 7]` | 81 | explicit padding (8-align) |
| `bids` | `[Order; 64]` | 88 | resting bids; 64 bytes each |
| `asks` | `[Order; 64]` | 4184 | resting asks; 64 bytes each |
| `events` | `[OutEvent; 128]` | 8280 | event-queue ring; 112 bytes each |
| `observations` | `[Observation; 16]` | 22616 | TWAP ring; 32 bytes each |

- `LEN = 23_128` (`size_of::<OrderBook>()`, excluding the 8-byte discriminator).
- Fixed capacities: `MAX_ORDERS_PER_SIDE = 64`, `EVENT_QUEUE_LEN = 128`,
  `TWAP_OBSERVATIONS = 16`. The book's side is implied by which array an `Order`
  sits in (`bids` vs `asks`), so no side byte is stored.
- Zero-copy is required because the account exceeds the SBF 4 KiB stack limit for
  borsh deserialization; handlers access it via `AccountLoader::load_mut()`.

### `Order` (sub-struct)

| Field | Type | Offset (payload) | Notes |
| --- | --- | --- | --- |
| `owner` | Pubkey | 0 | signer who placed the order |
| `price` | u64 | 32 | `APY_SCALE` fixed point; `0` invalid |
| `size` | u64 | 40 | remaining (unfilled) size, notional USDC microunits |
| `seq` | u64 | 48 | monotonic order id (time priority) |
| `active` | u8 | 56 | `0` = empty slot |
| `_pad` | `[u8; 7]` | 57 | explicit padding (8-align) |

- `LEN = 64`. `active` is `u8` (not `bool`) because `bytemuck::Pod` forbids `bool`.

### `OutEvent` (sub-struct)

| Field | Type | Offset (payload) | Notes |
| --- | --- | --- | --- |
| `seq` | u64 | 0 | monotonic event id |
| `price` | u64 | 8 | traded price (fill) or order price |
| `size` | u64 | 16 | traded size (fill) or remaining size |
| `owner` | Pubkey | 24 | order owner |
| `counterparty` | Pubkey | 56 | matched counterparty (zero when unset) |
| `entry_total_lamports` | u64 | 88 | fill-time index snapshot numerator (`total_lamports`); `0` on non-fill events |
| `entry_pool_token_supply` | u64 | 96 | fill-time index snapshot denominator (`pool_token_supply`); `0` on non-fill events |
| `settled` | u8 | 104 | `0` = Fill pending maker settlement; `1` = consumed by `settle_fill` (meaningless on other kinds) |
| `kind` | u8 | 105 | `0` = Fill, `1` = Cancel, `2` = Residual |
| `side` | u8 | 106 | `0` = Bid, `1` = Ask |
| `_pad` | `[u8; 5]` | 107 | explicit padding (8-align) |

- `LEN = 112`. The fill-time index snapshot (D7/D8) lets `settle_fill` book a
  resting maker at the rate that was in effect when the fill executed — not at
  settlement time — and the `settled` flag makes settlement idempotent.

### `Observation` (sub-struct)

| Field | Type | Offset (payload) | Notes |
| --- | --- | --- | --- |
| `slot` | u64 | 0 | slot at which the sample was recorded |
| `mid` | u64 | 8 | mid price in effect at this sample (`0` = one-sided/undefined) |
| `cumulative_mid` | `[u8; 16]` | 16 | running `Σ mid × Δslot` (LE `u128`) |

- `LEN = 32`. `cumulative_mid` is stored as 16 raw bytes (not `u128`) for
  cross-target alignment stability in the zero-copy layout.

## `UserCollateral` (anchor account)

One PDA per `(market, user)`, seed `[b"user_collateral", market.key(),
user.key()]`. Borsh layout (after the 8-byte discriminator):

| Field | Type | Offset (payload) | Notes |
| --- | --- | --- | --- |
| `deposited` | u64 | 0 | USDC credited to the user, microunits |
| `reserved` | u64 | 8 | USDC reserved for open positions; `= Σ` position collateral (issue #5) |
| `bump` | u8 | 16 | PDA bump |

- `LEN = 17`.
- Lazily initialized on first deposit; `reserved` is written by the position
  lifecycle (`open_position` / `close_position` / `settle_fill` reserve and
  release margin atomically with the `Position` ledger).

## `Position` (anchor account)

One PDA per `(market, user, side)`, seed
`[POSITION_SEED, market.key(), user.key(), side]` with
`POSITION_SEED = b"position"`. Borsh layout (after the 8-byte discriminator):

| Field | Type | Offset (payload) | Notes |
| --- | --- | --- | --- |
| `market` | Pubkey | 0 | bound market (also in the PDA seed) |
| `owner` | Pubkey | 32 | user holding the position (also in the PDA seed) |
| `side` | u8 | 64 | `0` = Long/Bid, `1` = Short/Ask (`SIDE_BID`/`SIDE_ASK`) |
| `notional` | u64 | 65 | remaining position, USDC microunits; `0` == closed |
| `entry_n_sum` | u128 | 73 | `Σ(total_lamports × fill_size)` — notional-weighted entry-index running sum |
| `entry_d_sum` | u128 | 89 | `Σ(pool_token_supply × fill_size)` — notional-weighted entry-index running sum |
| `collateral` | u64 | 105 | reserved margin = `margin_required(notional, initial_margin_bps)` (ceiling division) |
| `last_funding_epoch` | u64 | 113 | stored; `0` this iteration (#6 writes it) |
| `open_slot` | u64 | 121 | (re)creation slot: fill slot (inline taker opens) or settlement slot (maker re-opens via `settle_fill`) |
| `bump` | u8 | 129 | PDA bump |

- `LEN = 130`.
- Lazily created on first fill/settlement (payer = user/cranker) and **retained**
  after a full close; `notional == 0` means closed. A re-open resets
  `entry_n_sum` / `entry_d_sum` / `open_slot`.
- The entry index is stored as notional-weighted running sums (exact
  average-cost accounting, no intermediate rounding); the snapshot rate
  `entry_n_sum / entry_d_sum` is computed at PnL time after a shared
  power-of-two normalization (`positions::normalize_sums`).
- `u128` fields borsh-serialize to 16 LE bytes; as a plain borsh `#[account]`
  (well under the 4 KiB zero-copy threshold) the `Position` declares native
  `u128` fields — no per-access byte conversion.
- Margin is ledger-only: `collateral` mirrors the reserved-margin bookkeeping in
  `UserCollateral.reserved`; no token movement on open or close.

## `ExchangeRate` (derived, not stored)

| Field | Type | Source |
| --- | --- | --- |
| `total_lamports` | u64 | stake-pool account offset 258 |
| `pool_token_supply` | u64 | stake-pool account offset 266 |

- Rate = `total_lamports / pool_token_supply` (SOL per jitoSOL), kept as a
  rational pair to defer division until the yield computation.
- `realized_yield(t0, t1) = (rate_t1 / rate_t0 − 1) · 1_000_000`, computed with
  `u128` intermediates.

## Relationships

```mermaid
erDiagram
    YieldOracle ||--|| Publisher : "ed25519-signed"
    PerpMarket ||--|| StakePoolAccount : "index_source"
    PerpMarket ||--|| OrderBook : "book bound by market key"
    PerpMarket ||--o{ UserCollateral : "ledger per (market, user)"
    PerpMarket ||--o{ Position : "position per (market, user, side)"
    PerpMarket ||--|| VaultTokenAccount : "collateral custody (seed vault)"
    ExchangeRate ||--|| StakePoolAccount : "reads"
    YieldOracle {
        u64 apy
        u64 version
        u64 last_update_slot
        Pubkey publisher
        Pubkey authority
        u64 stale_after_slots
    }
    PerpMarket {
        Pubkey index_source
        Pubkey collateral_mint
        u64 funding_k
        u64 max_funding
        u64 funding_epoch_slots
        u16 initial_margin_bps
        u16 maintenance_margin_bps
        Pubkey authority
        Pubkey vault
    }
    OrderBook {
        Pubkey market
        u64 next_seq
        u64 best_bid
        u64 best_ask
        u64 event_read_cursor
        u64 event_write_cursor
        u64 twap_cursor
    }
    UserCollateral {
        u64 deposited
        u64 reserved
    }
    Position {
        Pubkey market
        Pubkey owner
        u8 side
        u64 notional
        u128 entry_n_sum
        u128 entry_d_sum
        u64 collateral
        u64 last_funding_epoch
        u64 open_slot
    }
    ExchangeRate {
        u64 total_lamports
        u64 pool_token_supply
    }
```

No database / migrations — state lives entirely in Solana accounts.
