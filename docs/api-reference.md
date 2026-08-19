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

## Signature Scheme (`update_apy`)

The transaction must include an `ed25519` program instruction (inline data,
`*_instruction_index == u16::MAX`) whose public key equals `oracle.publisher` and
whose 32-byte message equals:

```
sha256("fructus::update_apy" ‖ oracle_address ‖ apy_le(8) ‖ version_le(8))
```

The program introspects the transaction instruction list (via the instruction
sysvar) and binds that instruction to the expected publisher + message.
