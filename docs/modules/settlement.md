# Module: Settlement (trustless exchange rate)

**Purpose:** Derive jitoSOL realized yield from on-chain stake-pool state — no
oracle, no staleness, no manipulation.

## Public API

| Instruction | Signature | Description |
| --- | --- | --- |
| `read_exchange_rate` | `()` | Validate pool owner + discriminator, read rate, log it |

| Function | Signature | Description |
| --- | --- | --- |
| `ExchangeRate::read` | `(&[u8]) -> Option<ExchangeRate>` | Read `total_lamports` + `pool_token_supply` after validating `account_type` |
| `ExchangeRate::realized_yield` | `(&self, &Self) -> Option<u64>` | `(rate_t1/rate_t0 − 1) · APY_SCALE` |
| `annualize` | `(yield: u64, period_slots, slots_per_year) -> Option<u64>` | annualize a scaled yield |

## Data

| Constant | Value | Notes |
| --- | --- | --- |
| `ACCOUNT_TYPE_STAKE_POOL` | `1` | `AccountType::StakePool` |
| `TOTAL_LAMPORTS_OFFSET` | `258` | borsh layout **with** `account_type` prefix |
| `POOL_TOKEN_SUPPLY_OFFSET` | `266` | |
| `STAKE_POOL_PROGRAM_ID` | `SPoo1Ku8WFXoNDMHPsrGSTSG1Y47rzgn41SLUNakuHy` | canonical SPL stake pool |

## Dependencies

- Inbound: `lib.rs::read_exchange_rate`.
- Outbound: `constants` (`APY_SCALE`).

## Patterns & Gotchas

- **Offset correctness is critical** — the `account_type: u8` field shifts the
  classic layout by one byte. Offsets are verified against
  `solana-program/stake-pool` `state.rs`; do not "fix" 258/266 to 257/265.
- `realized_yield` clamps negative yield to `0` (defensive; impossible for a
  functioning LST) and returns `None` on zero denominators or overflow.
- `read_exchange_rate` does **not** store anything — it is a read/validate
  primitive; the futures contract will snapshot `ExchangeRate` at open and read
  it again at settle.
