# Module: Funding Engine (premium, funding rate, settle_funding)

**Purpose:** Anchor the order-book `mark` to the trustless `index` (the
annualized jitoSOL yield) so the perpetual trades at fair value. A positive
premium (mark above index) funds **longs → shorts**; the permissionless
`settle_funding` accrues the signed funding of a position's `notional` over the
full epochs that have elapsed since its last settlement. The math is a pure,
dependency-free module (`crate::funding`) locked by `proptest`; the thin
adapter in `lib.rs` applies it to the on-chain `PerpMarket` / `Position` /
`UserCollateral` accounts.

Units are `APY_SCALE = 1_000_000` fixed point (mark/index/premium/rate) unless
stated; `funding_payment` is signed USDC microunits.

## Public API

| Instruction | Signature | Description |
| --- | --- | --- |
| `settle_funding` | `()` | Permissionless per-position funding accrual over the full elapsed epochs (below) |

| Pure function | Signature | Description |
| --- | --- | --- |
| `premium` | `(mark: u64, index: u64) -> i128` | `mark − index`, signed (R-F1) |
| `funding_rate` | `(premium: i128, funding_k: u64, max_funding: u64) -> i128` | `clamp(funding_k·premium/APY_SCALE, −max_funding, +max_funding)`, signed (R-F2) |
| `funding_payment` | `(notional: u64, rate: i128, epochs: u64, side: SideFlow) -> i128` | `notional·rate/APY_SCALE · epochs × side_flow`, signed (R-F3/R-F5) |
| `funding_epoch` | `(slot: u64, epoch_slots: u64) -> u64` | `slot / epoch_slots`; returns `0` for a zero epoch length (R-F5) |

### `SideFlow`

| Variant | Flow | Meaning on positive funding |
| --- | --- | --- |
| `SideFlow::Long` | `−1` | pays (flow negative) |
| `SideFlow::Short` | `+1` | receives (flow positive) |

`SideFlow::from_position_side(PositionSide)` maps `Long`/`Short` to the
`Long`/`Short` flow, and `multiplier()` returns the signed `+1`/`−1`. The flow
sign is encoded **separately** from `PositionSide` so the convention is explicit
and property-testable: the issue's free-form "side: long=+1, short=−1" labels
the *position* side, whereas the *cash-flow* sign for a long on positive funding
is `−1`.

## Sign convention (R-F3)

```
premium = mark − index                                   (i128, signed)
funding_rate = clamp(funding_k·premium / APY_SCALE, −max_funding, +max_funding)
funding_payment = notional·rate / APY_SCALE · epochs × side_flow
```

- `premium > 0` (mark above index) ⇒ `funding_rate > 0` ⇒ **longs pay shorts**:
  `funding_payment(Long) < 0`, `funding_payment(Short) > 0`, exact opposites.
- `premium < 0` ⇒ the sign flips (shorts pay, longs receive).
- `premium == 0` (or `funding_k == 0`) ⇒ `rate == 0` ⇒ no funding.
- `funding_rate` is clamped symmetric about `0` and is monotonic in `premium`
  (odd: `rate(−p) = −rate(p)`).

## Epoch accounting (R-F5)

```
epoch = slot / funding_epoch_slots
epochs = cur_epoch − last_funding_epoch        (full epochs that have elapsed)
```

- Only **full** epochs that have *elapsed* are settled — a `cur_epoch` partial
  epoch contributes nothing until it completes.
- `epochs == 0` ⇒ `settle_funding` is an idempotent no-op: settling the same
  epoch twice adds nothing (re-entrancy-safe accrual).
- `funding_epoch(.., epoch_slots = 0)` returns `0` — a degenerate epoch length
  collapses every slot to epoch `0`, so nothing accrues spuriously. (Note:
  `funding_epoch_slots` is not validated at init; see
  [market.md](market.md).)

## The on-chain index (R-F5)

The `index` level is derived **trustlessly from the stake pool**, never an
oracle: `annualize(realized_yield(rate@baseline, rate@now), elapsed_slots,
SLOTS_PER_YEAR)`. The baseline is the market's last-settlement
`ExchangeRate` snapshot (`index_n`/`index_d`); the current rate is the live
pool read. On the **first** settlement (`index_d == 0`, no baseline) the handler
establishes the baseline and uses `index == 0` (no realized yield to annualize),
so `premium = mark − 0`.

`mark` is the order-book mid (`orderbook::mid`), falling back to `index` so a
one-sided/empty book yields `premium == 0` (no funding) rather than a spurious
spike.

## `PerpMarket` funding fields (R-F4)

The singleton market carries the accrual state (see
[data-models.md](../data-models.md) for full offsets):

| Field | Type | Notes |
| --- | --- | --- |
| `funding_epoch` | u64 | last settled epoch index; `0` before the first settlement |
| `index_n` | u64 | stake-pool rate snapshot **numerator** at the last settlement (the epoch baseline) |
| `index_d` | u64 | stake-pool rate snapshot **denominator**; `index_n/index_d == 0` marks an un-set baseline |
| `funding_accumulator` | i128 | cumulative signed funding realized on the market (net-additive; long flows negative, short positive) |

`Position.last_funding_epoch` (a `u64`) tracks the position's own settlement
point; `settle_funding` advances it to `cur_epoch` and the market baseline to the
live pool rate on settlement.

## `settle_funding` flow

1. Bind the supplied `position`/`user_collateral` to the market + the user's
   PDAs (byte-level; `PositionNotFound` / `InvalidAccountData` on mismatch).
2. Derive `cur_epoch = funding_epoch(now_slot, funding_epoch_slots)` and
   `epochs = cur_epoch − position.last_funding_epoch`; `epochs == 0` is an
   idempotent no-op.
3. Read the live pool rate; compute the trustless `index` from the market
   baseline (`annualize`); read the order-book `mid` as `mark` (fallback `index`).
4. `premium = mark − index`, `rate = funding_rate(premium, funding_k,
   max_funding)`, `payment = funding_payment(position.notional, rate, epochs,
   SideFlow::from_position_side(side))`.
5. Apply the signed `payment` to `UserCollateral.deposited` via
   `positions::apply_pnl` (a loss is clamped so `deposited` never goes negative).
6. Advance `position.last_funding_epoch = cur_epoch`, the market baseline to the
   live pool rate, and `market.funding_accumulator += payment`.

## Dependencies

- Inbound: `lib.rs::settle_funding` (the thin adapter).
- Outbound: `constants` (`APY_SCALE`, `SLOTS_PER_YEAR`), `positions`
  (`PositionSide`, `apply_pnl`), `exchange` (`ExchangeRate`, `realized_yield`,
  `annualize`), `orderbook` (`mid`), `state` (`PerpMarket`, `Position`,
  `UserCollateral`), `error`.

## Patterns & Gotchas

- **Signed arithmetic throughout** — `premium`, `rate`, and `payment` are
  negative half the time, so `funding.rs` lives on `i128` with
  `saturating_*`/`checked_*` (never unsigned `saturating_*`); AGENTS.md
  forbids panicking math.
- **Fixed-point convention** — `funding_k ∈ [1, APY_SCALE]` (validated at init),
  `max_funding ∈ [0, APY_SCALE]`. `rate` is `funding_k·premium/APY_SCALE`
  (single `i128` division, truncating toward zero so the sign is preserved).
- **Quantization floor** — `funding_payment` truncates after the per-epoch
  `notional·rate/APY_SCALE` division, so it is `0` whenever
  `|notional·rate/APY_SCALE| < 1` for a single epoch (documented floor; linear
  in `epochs` but for the per-epoch truncation).
- **`[INFERRED]`** — the MVP applies the **current** funding rate to all elapsed
  epochs (not a per-epoch premium history) — a deterministic approximation the
  accumulator makes net-additive.
- **Net-additive accumulator** — `funding_accumulator` sums every signed payment,
  so a long's contribution is negative and a short's positive; the market-wide
  running total shows the funding flow direction.
- **`settle_funding` takes the market-bound `index_source`** — it must
  byte-equal `market.index_source` and pass the stake-pool
  owner/discriminator validation before the trustless index is derived (R-F5).
  (Unlike `settle_fill`, which is open-intent and re-reads an event-carried
  snapshot, `settle_funding` derives the index live from the pool.)
