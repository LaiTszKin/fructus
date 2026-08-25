# Module: Settlement (trustless exchange rate + settle_close)

**Purpose:** Derive jitoSOL realized yield from on-chain stake-pool state — no
oracle, no staleness, no manipulation — and wire it into position settlement.
The module reads the stake-pool **exchange rate** (`total_lamports /
pool_token_supply`) as the trustless index and turns it into a signed PnL for the
notional a position has closed. `settle_close` realizes that PnL into the user's
collateral; the pure math lives in `positions` (see
[positions.md](positions.md) for `pnl` / `apply_pnl`), and this module owns the
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

`settle_close` depends on the pure `positions::pnl` (the signed PnL over a
notional at the current index) and `positions::apply_pnl` (the ledger transition
that credits a profit or debits — clamped — a loss). Both are owned by
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
margin, and records the closed amount in `Position.closed_notional` but settles
nothing. `settle_close` settles that notional against the market's live
stake-pool index — **never the mark oracle** (R-S2) — into
`UserCollateral.deposited` and resets `closed_notional` to `0`:

```
pnl        = positions::pnl(entry_n_sum, entry_d_sum, cur_n, cur_d, closed_notional, side)   (signed i128)
deposited' = positions::apply_pnl(deposited, pnl)                                           (clamped)
```

- **No-op on `closed_notional == 0`** — `settle_close` returns immediately
  (idempotent; R-S3).
- **Positive PnL credits** — `apply_pnl` adds the profit to `deposited`
  (returning `None` only on a positive `u64` overflow).
- **Negative PnL debits, clamped at `0`** — a loss reduces `deposited` but is
  clamped so it never goes below `0`; the vault is never left insolvent by a
  settlement (R-S3). `apply_pnl` is total on a loss (never `None`).
- **Trustless & dependency-minimal** — settlement depends only on `exchange.rs`
  data: the position's entry running sums (recorded at fill time) plus the
  **current** `read_stake_pool` read. The handler binds the supplied
  `position`/`user_collateral` to the user's PDAs byte-for-byte
  (`InvalidAccountData` on mismatch) and requires `position.market == market`.

## Dependencies

- Inbound: `lib.rs::read_exchange_rate`, `lib.rs::settle_close`.
- Outbound: `constants` (`APY_SCALE`, `SLOTS_PER_YEAR`), `positions`
  (`pnl`, `apply_pnl`, `PositionSide`), `exchange` (`ExchangeRate`,
  `realized_yield`, `annualize`), `state` (`Position`, `UserCollateral`,
  `PerpMarket`).

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
  no mark oracle; the vault is never insolvent because `apply_pnl` clamps a loss
  at the deposited balance.

