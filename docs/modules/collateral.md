# Module: Collateral Vault (vault token account + UserCollateral ledger)

**Purpose:** USDC custody for the perpetual market: a program-owned vault token
account plus a per-`(market, user)` ledger of `deposited` / `reserved`, with
`deposit_collateral` / `withdraw_collateral` moving funds and updating the ledger
atomically. The free-collateral seam (`free_collateral() = deposited − reserved`)
is the hook the position lifecycle (#5) will later use to keep collateral backing
open margin from being withdrawn.

## Public API

| Instruction | Signature | Description |
| --- | --- | --- |
| `initialize_collateral_vault` | `()` | Create the vault token account at `PerpMarket.vault` (authority-gated) |
| `deposit_collateral` | `(amount: u64)` | Move `amount` USDC user ATA → vault; credit the ledger |
| `withdraw_collateral` | `(amount: u64)` | Move `amount` USDC vault → user ATA; debit the ledger |

| Pure function | Signature | Description |
| --- | --- | --- |
| `free_collateral` | `(deposited: u64, reserved: u64) -> Option<u64>` | `deposited.checked_sub(reserved)`; `None` iff `reserved > deposited` |
| `deposit` | `(deposited: u64, amount: u64) -> Option<u64>` | `deposited.checked_add(amount)`; `None` on overflow |
| `withdraw` | `(deposited: u64, reserved: u64, amount: u64) -> Option<u64>` | `deposited.checked_sub(amount)` gated by `amount <= free_collateral` |

## Vault token account

- Located at the PDA already stored in `PerpMarket.vault`, seed
  `[VAULT_SEED]` = `[b"vault"]` (unchanged from issue #2 — it is **not**
  re-derived with a different seed).
- Created by `initialize_collateral_vault` in two CPI steps:
  1. `system_program::create_account` at the vault PDA (payer = signer, program
     owner = SPL Token), and
  2. `token::initialize_account3` with `mint = market.collateral_mint` and
     `authority = vault` — the vault **authorizes itself**.
- The collateral mint must be a Token-program mint with `decimals ==
  USDC_DECIMALS` (`6`), else `InvalidMint`. A second init (vault already holds
  token-account data) fails `VaultAlreadyInitialized`.
- Only the program can move funds out: the vault is a PDA, so transfers sign via
  its bump seeds.

## `UserCollateral` ledger

One PDA per `(market, user)`, seed
`[USER_COLLATERAL_SEED, market.key(), user.key()]` with
`USER_COLLATERAL_SEED = b"user_collateral"`. Both amounts are USDC microunits
(6 decimals).

| Field | Type | Notes |
| --- | --- | --- |
| `deposited` | u64 | USDC credited to the user, microunits |
| `reserved` | u64 | USDC reserved for open positions (stubbed to `0` this iteration) |
| `bump` | u8 | PDA bump |

- Lazily initialized on **first deposit** (payer = user), both fields zero.
- `reserved` has **no writer** this iteration (no positions yet), so
  `free_collateral() == deposited` always holds. The layout + offset is in
  [data-models.md](../data-models.md).

## Deposit / withdraw flow

**Deposit** (`deposit_collateral(amount)`, user-signed):

1. Reject `amount == 0` (`InvalidSize`).
2. Lazily system-create the `UserCollateral` PDA on first deposit (payer = user).
3. `token::transfer` `amount` USDC from the user's ATA into the vault (authority
   = the user signer).
4. `deposited += amount` via `checked_add` (`ArithmeticOverflow` on overflow).

**Withdraw** (`withdraw_collateral(amount)`, user-signed):

1. Reject `amount == 0` (`InvalidSize`).
2. Enforce `amount <= free_collateral(deposited, reserved)`
   (`InsufficientFreeCollateral` otherwise).
3. `token::transfer` `amount` USDC from the vault to the user's ATA (authority =
   the vault PDA, signing via `[VAULT_SEED, bump]`).
4. `deposited -= amount` via `checked_sub` (`ArithmeticOverflow` on overflow).

Both are atomic — any error unwinds the transfer and the ledger write.

## `free_collateral()` seam

`free_collateral(deposited, reserved) = deposited − reserved` is the single
predicate every withdrawal checks. It is a pure, property-tested function and
returns `None` only on the invariant violation `reserved > deposited`. Because
`reserved` is stubbed to `0`, the check reduces to `amount <= deposited` today;
when issue #5 raises `reserved` for open positions, the **same** check will
reject withdrawing collateral that still backs margin — no new withdrawal path is
needed.

## Dependencies

- Inbound: `lib.rs` (`initialize_collateral_vault`, `deposit_collateral`,
  `withdraw_collateral`).
- Outbound: `constants` (`VAULT_SEED`, `USER_COLLATERAL_SEED`,
  `USDC_DECIMALS`), `error`, `state` (`UserCollateral`, `PerpMarket`),
  `anchor-spl` (`token::transfer`, `token::initialize_account3`,
  `associated_token`).

## Patterns & Gotchas

- **Pure accounting, thin adapter** — `collateral.rs` operates on plain `u64`
  values so `proptest` drives the invariants directly; `lib.rs` applies them to
  the on-chain ledger.
- **Vault is self-authorized** — `initialize_account3` sets `authority = vault`
  (the account being initialized), so the explicit two-CPI path is used rather
  than the `#[account(init, token::authority=…)]` shortcut (self-referential in
  the Accounts derive).
- **Deposit/withdraw are permissionless for the owning user**, but vault creation
  is authority-gated (`market.authority`).
- **Plain `token::transfer`, not `transfer_checked`** — the mint is already
  validated at vault initialization, so plain transfer is cheaper and sufficient.
