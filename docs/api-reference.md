# API Reference (On-chain Instructions)

Program id: `8ZLiJ12eBiam4UP2HRp3M75CQAcc8GuUBz44zeHt6mjH`

All instructions are Anchor handlers. Accounts are validated by Anchor
constraints + explicit checks in the handler.

| Instruction | Args | Required Accounts | Auth | Effect |
| --- | --- | --- | --- | --- |
| `initialize` | `publisher: Pubkey`, `stale_after_slots: u64`, `initial_apy: u64` | `oracle` (init PDA), `authority` (signer, payer), `system_program` | — | Create oracle; set fields; `initial_apy` must be ≤ `MAX_APY` |
| `update_apy` | `apy: u64`, `version: u64` | `oracle` (mut), `instruction_sysvar` | publisher (ed25519 sig) | Verify sig; `version` must be `> current`; `apy ≤ MAX_APY`; set `apy/version/last_update_slot` |
| `set_stale_window` | `new_stale_after_slots: u64` | `oracle` (mut), `authority` (signer) | `authority` | Update `stale_after_slots` |
| `set_publisher` | `new_publisher: Pubkey` | `oracle` (mut), `authority` (signer) | `authority` | Rotate `publisher` |
| `read_exchange_rate` | — | `stake_pool` (unchecked) | — | Require owner == stake-pool program + `account_type == StakePool`; read + log rate |
| `initialize_market` | `collateral_mint: Pubkey`, `funding_k: u64`, `max_funding: u64`, `funding_epoch_slots: u64`, `initial_margin_bps: u16`, `maintenance_margin_bps: u16` | `market` (init PDA), `index_source` (unchecked), `authority` (signer), `payer` (signer, mut), `system_program` | `authority` (admin) | Create singleton market; validate `index_source` (owner + `StakePool` discriminator) and numeric bounds; derive + store `vault` PDA; set all fields |
| `initialize_order_book` | — | `order_book` (init PDA), `market` (has_one authority), `authority` (signer), `payer` (signer, mut), `system_program` | `authority` == `market.authority` | Create the market-bound `OrderBook` PDA (seed `[b"order_book", market]`); zero the header, cursors, and inline arrays |
| `place_limit_order` | `side: u8`, `price: u64`, `size: u64` | `order_book` (mut), `market`, `owner` (signer) | `owner` (self) | Reject zero `price`/`size`; rest the order if non-crossing, otherwise match inline (bounded by `MAX_MATCH_STEPS`); never rest a crossing order |
| `place_market_order` | `side: u8`, `size: u64` | `order_book` (mut), `market`, `owner` (signer) | `owner` (self) | Cross the opposite book best-price-first until filled or exhausted; IOC — unfilled remainder cancelled, never over-fills |
| `cancel_order` | `seq: u64` | `order_book` (mut), `market`, `owner` (signer) | `owner` only | Remove one resting order (owner-only); append a `Cancel` event |
| `crank` | — | `order_book` (mut), `market`, `cranker` (signer) | permissionless | Drain up to `CRANK_BATCH_LEN` (8) events; emit `Fill`/`Cancel`, re-match `Residual` entries; never matches off-chain |
| `initialize_collateral_vault` | — | `market` (has_one authority), `authority` (signer), `payer` (signer, mut), `vault` (mut, seed `[b"vault"]`), `collateral_mint` (Mint == `market.collateral_mint`), `system_program`, `token_program` | `authority` == `market.authority` | System-create + `initialize_account3` the vault token account with itself as authority; validate mint `decimals == 6`; fail `VaultAlreadyInitialized` on retry |
| `deposit_collateral` | `amount: u64` | `user` (signer, mut), `market`, `user_collateral` (mut, lazily created), `vault` (mut), `user_ata` (mut ATA), `collateral_mint` (Mint), `system_program`, `token_program` | `user` | Lazily create the ledger on first deposit; `token::transfer` ATA → vault; `deposited += amount` (checked) |
| `withdraw_collateral` | `amount: u64` | `user` (signer), `market`, `user_collateral` (mut), `vault` (mut), `user_ata` (mut ATA), `collateral_mint` (Mint), `token_program` | `user` + vault PDA (signs via seeds) | Enforce `amount <= deposited - reserved`; `token::transfer` vault → ATA; `deposited -= amount` (checked) |

## Errors

| Code | Variant | Trigger |
| --- | --- | --- |
| — | `ApyTooHigh` | `apy > 1_000_000` on `initialize` / `update_apy` |
| — | `StaleVersion` | `version <= oracle.version` |
| — | `InvalidSignature` | ed25519 pubkey/message mismatch or malformed instruction |
| — | `SignatureMissing` | no matching ed25519 verify instruction in transaction |
| — | `InvalidStakePool` | wrong owner or discriminator in `read_exchange_rate` / `initialize_market` `index_source` validation |
| — | `InvalidFundingK` | `funding_k` outside `[1, 1_000_000]` on `initialize_market` |
| — | `InvalidMaxFunding` | `max_funding > 1_000_000` on `initialize_market` |
| — | `InvalidInitialMargin` | `initial_margin_bps` outside `(0, 10_000]` on `initialize_market` |
| — | `InvalidMaintenanceMargin` | `maintenance_margin_bps` not in `(0, initial_margin_bps]` on `initialize_market` |
| — | `BookFull` | limit order on a side already at `MAX_ORDERS_PER_SIDE` (64); or a deferred `Residual` that cannot be queued because the event ring is full (backpressure) |
| — | `BookAlreadyInitialized` | second `initialize_order_book` (the book account already holds data) |
| — | `BookNotInitialized` | book op (`place_limit_order`/`place_market_order`/`cancel_order`/`crank`) before `initialize_order_book` |
| — | `InvalidPrice` | zero `price`, or a crossing limit that cannot rest |
| — | `InvalidSize` | zero `size` on `place_limit_order`/`place_market_order`, or zero `amount` on deposit/withdraw |
| — | `OrderNotFound` | `cancel_order` on a non-existent or already-filled `seq` |
| — | `OrderOwnerMismatch` | `cancel_order` by a non-owner |
| — | `SelfTrade` | a crossing limit whose only crossable maker is self-owned and nothing else filled |
| — | `InvalidMint` | collateral mint `decimals != 6` on `initialize_collateral_vault` |
| — | `InsufficientFreeCollateral` | `withdraw_collateral` with `amount > deposited - reserved` |
| — | `VaultAlreadyInitialized` | second `initialize_collateral_vault` (vault already holds token-account data) |
| — | `VaultNotInitialized` | deposit/withdraw before `initialize_collateral_vault` |
| — | `ArithmeticOverflow` | checked-arithmetic overflow in `next_seq`/cursor increments or `deposited` add/sub |

`BookAlreadyInitialized`, `BookNotInitialized`, and `VaultNotInitialized` are
wired with explicit `require!` checks in the handlers: an account is treated as
initialized once it holds non-empty data, so a re-init or a pre-init access fails
with the dedicated variant rather than an Anchor account-constraint error.

## Signature Scheme (`update_apy`)

The transaction must include an `ed25519` program instruction (inline data,
`*_instruction_index == u16::MAX`) whose public key equals `oracle.publisher` and
whose 32-byte message equals:

```
sha256("fructus::update_apy" ‖ oracle_address ‖ apy_le(8) ‖ version_le(8))
```

The program introspects the transaction instruction list (via the instruction
sysvar) and binds that instruction to the expected publisher + message.
