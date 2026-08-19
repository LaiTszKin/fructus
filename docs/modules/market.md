# Module: Perp Market (PerpMarket + initialize_market)

**Purpose:** Singleton perpetual-market configuration that binds a trustless
index source to a collateral token and records funding + margin parameters.

## Public API

| Instruction | Signature | Description |
| --- | --- | --- |
| `initialize_market` | `(collateral_mint: Pubkey, funding_k: u64, max_funding: u64, funding_epoch_slots: u64, initial_margin_bps: u16, maintenance_margin_bps: u16)` | Create the singleton market PDA; validate config + `index_source`; derive vault PDA |

| Pure function | Signature | Description |
| --- | --- | --- |
| `funding_k_in_bounds` | `(k: u64) -> bool` | `k ∈ [1, 1_000_000]` |
| `max_funding_in_bounds` | `(m: u64) -> bool` | `m ∈ [0, 1_000_000]` |
| `initial_margin_in_bounds` | `(im: u16) -> bool` | `im ∈ (0, 10_000]` |
| `maintenance_margin_in_bounds` | `(im: u16, mm: u16) -> bool` | `mm ∈ (0, im]` |

## Account

`PerpMarket` (singleton PDA, seed `"perp_market"`):

| Field | Type | Notes |
| --- | --- | --- |
| `index_source` | Pubkey | jitoSOL SPL Stake Pool account (validated at init) |
| `collateral_mint` | Pubkey | USDC mint (stored as-is, no on-chain check) |
| `funding_k` | u64 | convergence speed; fixed-point scale `1_000_000` |
| `max_funding` | u64 | per-epoch funding-rate cap; fixed-point scale `1_000_000` |
| `funding_epoch_slots` | u64 | epoch length in slots |
| `initial_margin_bps` | u16 | initial margin, basis points |
| `maintenance_margin_bps` | u16 | maintenance margin, basis points |
| `authority` | Pubkey | admin |
| `vault` | Pubkey | collateral-custody PDA (derived, not created at init) |
| `bump` | u8 | market PDA bump |

## Dependencies

- Inbound: `lib.rs::initialize_market`.
- Outbound: `constants` (seeds, bounds), `error`, `state` (validators),
  `exchange` (`STAKE_POOL_PROGRAM_ID`, `ExchangeRate::read`).

## Patterns & Gotchas

- **Singleton for Stage 1** — seed `"perp_market"`; multi-market (Stage 3) will
  parameterize the seed. `index_source`/`collateral_mint` are plain fields, not
  seed components.
- **Validation is pure + testable** — the four bounds are pure bool predicates
  (like `apy_in_bounds`), mapped to errors in the handler via `require!`.
- **`index_source` is validated, not trusted** — owner must equal
  `STAKE_POOL_PROGRAM_ID` and `ExchangeRate::read` must succeed (discriminator
  check), via the shared `read_stake_pool` helper.
- **Vault is derived, not created** — `initialize_market` stores the vault PDA
  (seed `"vault"`) but does not create the token account (a later issue).
- `funding_epoch_slots` has no bound and `collateral_mint` is not validated in
  this iteration (documented out of scope).
