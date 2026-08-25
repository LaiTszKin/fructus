# Module: Order Book (OrderBook + matching engine + mark/twap)

**Purpose:** A fully on-chain limit order book (CLOB). One `OrderBook` PDA per
market holds the complete bid/ask book, a bounded event queue, and a TWAP
accumulator **inline** — no per-order PDA accounts, no off-chain book, no
oracle. The matching engine is a pure, dependency-free Rust module
(`crate::orderbook`) so its invariants are locked by `proptest` before any
instruction runs. `mid()` is the book-derived mark; `twap()` is the windowed
time-weighted mid primitive that liquidation (#8) consumes. Every `Fill`
event carries the **fill-time index snapshot** (`entry_total_lamports` /
`entry_pool_token_supply`) plus a `settled` flag so the position lifecycle (#5)
can book resting makers at the rate in effect when they filled.

## Public API

| Instruction | Signature | Description |
| --- | --- | --- |
| `initialize_order_book` | `()` | Create the market-bound `OrderBook` PDA (authority-gated) |
| `place_limit_order` | `(side: u8, price: u64, size: u64)` | Post a limit order: rest if non-crossing, otherwise match inline; every `Fill` stamped with the in-tx index snapshot |
| `place_market_order` | `(side: u8, size: u64)` | Cross best-price-first; immediate-or-cancel, never over-fills; `Fill`s stamped with the in-tx index snapshot |
| `cancel_order` | `(seq: u64)` | Remove one resting order (owner-only) |
| `crank` | `()` | Permissionless: drain the event queue + resume budget-interrupted takers; `Fill`s from a resumed residual stamped with the in-tx index snapshot |

| Pure function | Signature | Description |
| --- | --- | --- |
| `is_crossable` | `(bid: u64, ask: u64) -> bool` | `bid >= ask` (equal prices cross) |
| `price_better` | `(cand: u64, best: u64, side: Side) -> bool` | bids improve by rising (`cand > best`); asks by falling (`cand < best`) |
| `best_bid` / `best_ask` | `(&Book) -> u64` | max active bid / min active ask price; `0` when that side is empty |
| `mid` | `(&Book) -> Option<u64>` | `(best_bid + best_ask) / 2`, truncating; `None` when one-sided |
| `match_order` | `(&mut Book, incoming: Order, kind: OrderKind, max_steps: u64) -> MatchOutcome` | the matching engine (below) |
| `post_limit` | `(&mut Book, order: Order) -> Result<()>` | rest only if non-crossing, price ≠ 0, and below capacity |
| `cancel` | `(&mut Book, owner: Pubkey, seq: u64) -> Result<()>` | owner-only removal of exactly one order |
| `twap` | `(obs: &[Observation], window_slots: u64, now_slot: u64) -> Option<u64>` | time-weighted mid over a trailing window |

## Order & price representation

- **Price** is the traded yield level in `APY_SCALE` (`1_000_000`) fixed point,
  with a tick size of one micro-unit. `1.0` == `1_000_000`; the zero price is
  **invalid** for a live order (`InvalidPrice`). Price is stored as `u64`.
- **Size** is notional USDC microunits (6 decimals). Issue #5 reuses this exact
  unit for position notionals — the matching invariants stay unit-agnostic, so
  the notional mapping never touches the engine.
- **`seq`** is a monotonically increasing order id (`OrderBook.next_seq`),
  assigned on acceptance and used as the tie-breaker for time priority.
- **Side** is implied by which array a resting order sits in (`bids` vs `asks`)
  on-chain; the pure `orderbook::Order` carries an explicit `Side`.
- `order_type` is not stored on the resting order: only limit orders rest;
  market orders are immediate-or-cancel and never persist.

## The book (`OrderBook`)

One PDA per market, seed `[ORDER_BOOK_SEED, market.key()]` with
`ORDER_BOOK_SEED = b"order_book"`, binding each book to exactly one market.

| Field | Type | Notes |
| --- | --- | --- |
| `market` | Pubkey | the bound market (also in the PDA seed) |
| `bump` | u8 | PDA bump |
| `next_seq` | u64 | monotonic order id; incremented on every accepted order |
| `best_bid` | u64 | highest resting bid; `0` = bid side empty |
| `best_ask` | u64 | lowest resting ask; `0` = ask side empty |
| `event_read_cursor` | u64 | index of the next event to drain |
| `event_write_cursor` | u64 | index of the next event slot to write |
| `twap_cursor` | u64 | index of the next TWAP observation slot |
| `bids` | `[Order; 64]` | resting bids (side implied) |
| `asks` | `[Order; 64]` | resting asks (side implied) |
| `events` | `[OutEvent; 128]` | bounded event-queue ring |
| `observations` | `[Observation; 16]` | TWAP ring |

Each `Order` slot is `{ owner: Pubkey, price: u64, size: u64, seq: u64,
active: u8, _pad: [u8; 7] }` (zero-copy `#[repr(C)]` layout). The `active` flag
(`0` = empty, `1` = live; `u8` because `bytemuck::Pod` forbids `bool`)
distinguishes an empty slot from a live order, so `price == 0` stays a
*rejected* price rather than an ambiguity with an empty slot. `size` is the
**remaining** (unfilled) size. Full layouts and payload offsets are in
[data-models.md](../data-models.md).

Fixed capacities: `MAX_ORDERS_PER_SIDE = 64` (each resting order may occupy its
own price level, since the book uses flat per-side arrays with a price-time
comparator), `EVENT_QUEUE_LEN = 128`, `TWAP_OBSERVATIONS = 16`. Total payload is
`23_128` bytes (`OrderBook::LEN`) — up from `21_080` because each `OutEvent`
grew 96 → 112 bytes (the fill-time index snapshot + `settled` flag, issue #5);
the account is zero-copy (`#[account(zero_copy)]`) because it exceeds the SBF
4 KiB stack limit for borsh deserialization — handlers access it in place via
`AccountLoader::load_mut()`.

## Matching engine

`match_order` runs the taker against the **opposite** side:

1. **Price-time priority** — the best-priced maker fills first (highest bid /
   lowest ask), and at equal price the lowest `seq` (earliest) fills first.
2. **Partial fills** — each fill is `min(taker remaining, maker remaining)`; the
   maker's `size` is reduced, and a fully-filled maker is removed.
3. **No over-fill** — `Σ fills.size <= incoming.size`, and each fill is `<=` its
   maker's pre-fill size. Remaining sizes never go negative.
4. **No self-trade** — a resting maker owned by the taker is skipped and the
   next-best maker fills instead.
5. **Bounded batch** — a limit taker stops after `MAX_MATCH_STEPS = 8` fills,
   leaving any still-crossable remainder as a `Residual` for the crank (state is
   never left corrupt). A market taker matches to exhaustion (bounded by the 64
   makers per side) and is IOC, so it never leaves a residual.

`place_limit_order` posts a non-crossing order; a crossing order is matched
inline and never rests crossing. Its budget-interrupted remainder is re-queued as
a `Residual`; an unfilled, no-longer-crossing remainder rests; a remainder that
*still* crosses can only be because the crossing maker is self-owned. That
self-crossing remainder is cancelled when non-self fills already happened (so a
self-owned resting order never blocks other fills) and is rejected with
`SelfTrade` only when nothing filled. A remainder that cannot rest because its
side is at capacity is cancelled rather than reverting the whole transaction.

## Event queue & crank

Every book mutation appends an `OutEvent` to the bounded 128-entry ring:

| `kind` | Value | Emitted when |
| --- | --- | --- |
| `Fill` | `0` | a maker is filled |
| `Cancel` | `1` | a resting order is cancelled |
| `Residual` | `2` | a budget-interrupted limit taker's remainder is deferred |

`OutEvent` carries `seq` (monotonic), `owner`, `counterparty`, `side` (`0` =
Bid, `1` = Ask), `price`, and `size`; a `Fill` additionally carries the
**fill-time index snapshot** (`entry_total_lamports` / `entry_pool_token_supply`,
stamped by the fill-producing instruction) and a `settled` flag (`0` = pending
maker settlement, flipped to `1` by `settle_fill`). The write cursor assigns the
sequence and wraps around the ring.

Every fill-producing instruction — `place_limit_order`, `place_market_order`,
`crank`, and the position instructions `open_position` / `close_position` (issue
#5) — takes the **market-bound `index_source`** account (readonly; must
byte-equal `market.index_source`, plus stake-pool owner/discriminator
validation) and stamps its in-transaction exchange-rate snapshot verbatim onto
every `Fill` it emits, so a resting maker is settled at the rate that was in
effect when the fill executed. `cancel_order` is not fill-producing and takes no
`index_source`. A `Fill` that cannot be appended (full ring) fails the whole
transaction with `BookFull` — fills are never silently dropped (D10).

`crank` is **permissionless** (any signer) and drains up to `CRANK_BATCH_LEN = 8`
events per call: `Fill`/`Cancel` are emitted (logged) and consumed, while a
`Residual` is re-matched against the (now crossable) book — which may again
defer a residual. A `Residual` the engine cannot finish (a pure self-trade, or a
remainder that cannot rest) is cancelled and logged rather than reverting the
transaction, so the shared crank is never wedged. Fills a resumed residual
produces are stamped with the crank's in-transaction snapshot. The crank takes
**no position accounts** — residuals arise only from the position-neutral
`place_limit_order`, so it never settles positions. It never matches off-chain,
holds no privileged state, and cannot mint or move value.

## mark() / twap()

- **`mid()` (the mark)** — `(best_bid + best_ask) / 2` computed in `u128` and
  cast back to `u64`, so the sum cannot overflow. Division is **truncating
  toward zero** (integer floor on unsigned values). The result is always within
  `[best_bid, best_ask]` inclusive. It returns **`None` when the book is
  one-sided or empty** (`best_bid == 0 || best_ask == 0`); the index-source
  fallback for funding is `settle_funding`'s boundary (see
  [funding.md](funding.md)), not this module's.
- **`record_observation`** — on every book mutation, appends
  `cumulative_mid += previous_mid × Δslot` (`u128`, saturating): the elapsed
  interval saw the **pre-mutation** mid, so that is what is charged, never the
  post-mutation mid the caller records. The first sample starts at cumulative
  `0`; a `None` mid (one-sided book) contributes `0`, so an undefined mid never
  pollutes the accumulator.
- **`twap(obs, window_slots, now_slot)`** —
  `(cum_at(now_slot) − cum_at(now_slot − window_slots)) / window_slots`,
  truncating. Returns **`None`** for a zero window, empty history, a history
  that does not reach back a full window (no exact sample at either endpoint),
  a `u128` underflow, or a `u128 → u64` overflow. Purely deterministic and
  overflow-safe.

## Dependencies

- Inbound: `lib.rs` (`initialize_order_book`, `place_limit_order`,
  `place_market_order`, `cancel_order`, `crank`).
- Outbound: `constants` (seeds + capacities + `MAX_MATCH_STEPS`), `error`,
  `state` (`OrderBook`, `Order`, `OutEvent`, `Observation`).

## Patterns & Gotchas

- **Pure engine, thin adapter** — `orderbook.rs` works over in-memory `Vec`s and
  has no Anchor account plumbing; `lib.rs` `load_book`/`save_book` converts the
  fixed-capacity on-chain arrays, and recomputes `best_bid`/`best_ask` on every
  save. This mirrors the `exchange.rs` split and keeps the engine `proptest`-able.
- **A crossing order never rests** — the engine matches it inline or re-queues it
  as a `Residual`; the only way a limit remainder still crosses after matching is
  a self-owned maker, which is rejected with `SelfTrade` only when nothing else
  filled (a self-crossing remainder with fills is cancelled instead).
- **Market orders are IOC** — an unfilled remainder is silently cancelled, never
  posted, and never over-fills.
- **Compute bound is a fixed step budget** (`MAX_MATCH_STEPS = 8`), not runtime CU
  introspection — deterministic and testable; a worst-case full book (64 makers)
  drains in at most 8 cranks.
- **Event `seq` uses the write cursor** — monotonic across wraps; the read cursor
  advances only in `crank`.
- **Fills are never silently dropped** — a `Fill` that cannot be appended to a
  full ring fails the fill-producing transaction with `BookFull` (the taker
  retries after a crank), so an executed fill always persists its event and the
  maker can always settle (liveness bound in [positions.md](positions.md)).
- **`index_source` is market-bound** — every fill-producing instruction takes
  the readonly account and requires it to byte-equal `market.index_source`; the
  snapshot is read once per tx and stamped onto every `Fill`, so no instruction
  can stamp a non-canonical rate.
- **The book stays position-neutral** — `place_*`/`crank` never touch a taker's
  position; they only stamp fills for the maker's later `settle_fill`.
