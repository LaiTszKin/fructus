# Module: Settlement (trustless exchange rate + settle_close)

**Purpose:** Derive jitoSOL realized yield from on-chain stake-pool state — no
oracle, no staleness, no manipulation — and wire it into position settlement.
The module reads the stake-pool **exchange rate** (`total_lamports /
pool_token_supply`) as the trustless index and turns it into a signed PnL for the
notional a position has closed. `settle_close` realizes that PnL into the user's
collateral; the PnL math lives in `positions` (see
[positions.md](positions.md) for `pnl`; its counterparty netting runs through the
Design A `settlement` pool — see below), and this module owns the
**rate + annualization** primitives and the `settle_close` wiring.

## Public API

| Instruction | Signature | Description |
| --- | --- | --- |
| `read_exchange_rate` | `()` | Validate pool owner + discriminator, read rate, log it |
| `settle_close` | `()` | Permissionless: realize the signed index-based PnL of a position's `closed_notional` into `UserCollateral.deposited` (R-S2/R-S3) |

| Function | Signature | Description |
| --- | --- | --- |
| `ExchangeRate::read` | `(&[u8]) -> Option<ExchangeRate>` | Read `total_lamports` + `pool_token_supply` after validating `account_type` |
| `ExchangeRate::realized_yield` | `(&self, &Self) -> Option<u64>` | `(rate_t1/rate_t0 − 1) · APY_SCALE` |
| `annualize` | `(yield: u64, period_slots, slots_per_year) -> Option<u64>` | annualize a scaled yield |
| `settlement::apply_debit` | `(deposited, pool, debit) -> Option<(u64, u64)>` | collect a loser's debit into the pool (clamped at `deposited`) |
| `settlement::apply_credit` | `(deposited, claimable, pool, credit) -> Option<(u64, u64, u64)>` | pay a winner `min(credit, pool)`; remainder → `claimable` |
| `settlement::claim_payout` | `(deposited, claimable, pool) -> Option<(u64, u64, u64)>` | convert a claim into `deposited` up to the pool |
| `settlement::apply_liquidation_loss` | `(deposited, reserved_after, loss, reward) -> Option<(u64, u64)>` | book a victim's realized loss into the pool (bounded by the free seam + released margin − reward) |
| `settlement::settle_signed` | `(deposited, claimable, pool, signed_pnl) -> Option<(u64, u64, u64)>` | route a signed PnL through debit/credit (the handler wiring) |

`settle_close` depends on the pure `positions::pnl` (the signed PnL over a
notional at the current index) and the Design A `settlement` pool (the
counterparty-netted ledger transition). `positions` is owned by
[positions.md](positions.md); this module documents how settlement wires them.

## Data

| Constant | Value | Notes |
| --- | --- | --- |
| `ACCOUNT_TYPE_STAKE_POOL` | `1` | `AccountType::StakePool` |
| `TOTAL_LAMPORTS_OFFSET` | `258` | borsh layout **with** `account_type` prefix |
| `POOL_TOKEN_SUPPLY_OFFSET` | `266` | |
| `STAKE_POOL_PROGRAM_ID` | `SPoo1Ku8WFXoNDMHPsrGSTSG1Y47rzgn41SLUNakuHy` | canonical SPL stake pool |

## `settle_close` (R-S2/R-S3)

`close_position` is **lifecycle-only** (D4): it reduces `notional`, releases
margin, and records the closed amount in `Position.closed_notional` (plus its
**close-time** entry basis in `closed_entry_n_sum` / `closed_entry_d_sum`) but
settles nothing. `settle_close` settles that notional against the market's live
stake-pool index — **never the mark oracle** (R-S2) — into
`UserCollateral.deposited` and resets `closed_notional` (and `closed_entry_*`)
to `0`. `apply_close_fills` **accumulates** (not overwrites) the closed-entry
running sums as a **notional-weighted harmonic mean** of each closed
generation's close-time basis, so a sequence of closes interleaved with re-opens
(before any `settle_close`) prices **each** closed amount at **its own**
close-time basis — never the newest generation's (R-S1/R-S2):

```
pnl        = positions::pnl(closed_entry_n_sum, closed_entry_d_sum, cur_n, cur_d, closed_notional, side)   (signed i128)
// Design A PnL pool + pending claims (settlement::settle_signed):
//   pnl <= 0: apply_debit(deposited, pool, |pnl|)   -> collected into the pool (clamped at deposited)
//   pnl >  0: apply_credit(deposited, claimable, pool, pnl)  -> paid = min(pnl, pool); remainder -> claimable
(deposited', claimable', pool') = settlement::settle_signed(deposited, claimable, pool, pnl)
```

- **No-op on `closed_notional == 0`** — `settle_close` returns immediately
  (idempotent; R-S3).
- **A loss is collected into the pool** — the loser's debit is bounded by its
  `deposited` (clamped at `0`, never negative), and the collected amount is added
  to `PerpMarket.pnl_pool`.
- **A profit is paid only up to the pool** — `paid = min(credit, pool)`; the
  unfunded remainder becomes a **pending claim** (`claimable`), never
  `deposited`, so a winner is never minted unbacked collateral (Design A — the
  fix for the unbounded `apply_pnl` credit). The vault is never left insolvent
  (R-S3).
- **Trustless & dependency-minimal** — settlement depends only on `exchange.rs`
  data: the position's **close-time** entry basis (`closed_entry_n_sum` /
  `closed_entry_d_sum`, accumulated by `apply_close_fills` via
  `positions::accumulate_closed_entry`) plus the **current** `read_stake_pool`
  read, never the live entry sums (a re-open resets those).
  The handler binds the supplied
  `position`/`user_collateral` to the user's PDAs byte-for-byte
  (`InvalidAccountData` on mismatch) and requires `position.market == market`.

## Dependencies

- Inbound: `lib.rs::read_exchange_rate`, `lib.rs::settle_close`.
- Outbound: `constants` (`APY_SCALE`, `SLOTS_PER_YEAR`), `positions`
  (`pnl`, `PositionSide`), `settlement` (`settle_signed`, `apply_debit`,
  `apply_credit`, `claim_payout`, `apply_liquidation_loss`), `exchange`
  (`ExchangeRate`, `realized_yield`, `annualize`), `state` (`Position`,
  `UserCollateral`, `PerpMarket`).

## Patterns & Gotchas

- **Offset correctness is critical** — the `account_type: u8` field shifts the
  classic layout by one byte. Offsets are verified against
  `solana-program/stake-pool` `state.rs`; do not "fix" 258/266 to 257/265.
- `realized_yield` clamps negative yield to `0` (defensive; impossible for a
  functioning LST) and returns `None` on zero denominators or overflow.
- `read_exchange_rate` does **not** store anything — it is a read/validate
  primitive; a position snapshots `ExchangeRate` at open (the entry running sums)
  and `settle_close` reads it again at settle.
- **`settle_close` is permissionless and index-only** — no signer, no authority,
  no mark oracle; a loss is collected into the pool (clamped at `deposited`, so
  the vault is never left insolvent) and a profit is paid only up to the pool
  (the Design A anti-mint guarantee).

