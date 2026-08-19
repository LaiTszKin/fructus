# Module: Yield Oracle (mark-price APY)

**Purpose:** On-chain mark-price APY reference, updated only by publisher-signed data.

## Public API

| Instruction | Signature | Description |
| --- | --- | --- |
| `initialize` | `(publisher: Pubkey, stale_after_slots: u64, initial_apy: u64)` | Create the singleton oracle PDA |
| `update_apy` | `(apy: u64, version: u64)` | Apply a publisher-signed APY update |
| `set_stale_window` | `(new_stale_after_slots: u64)` | Admin: change staleness window |
| `set_publisher` | `(new_publisher: Pubkey)` | Admin: rotate publisher key |

| Pure function | Signature | Description |
| --- | --- | --- |
| `is_stale` | `(last, window, cur: u64) -> bool` | `cur.saturating_sub(last) >= window` |
| `apy_in_bounds` | `(apy: u64) -> bool` | `apy <= MAX_APY` |
| `validate_version` | `(current, next: u64) -> Result<()>` | rejects `next <= current` |
| `update_message` | `(&Pubkey, u64, u64) -> [u8; 32]` | canonical sha256 the publisher signs |

## Account

`YieldOracle` (singleton PDA, seed `"yield_oracle"`):

| Field | Type | Notes |
| --- | --- | --- |
| `apy` | u64 | scaled by `1_000_000` |
| `version` | u64 | monotonic; strictly increases per accepted update |
| `last_update_slot` | u64 | set to `Clock::slot` on update |
| `publisher` | Pubkey | authorized signer |
| `authority` | Pubkey | admin for `set_*` |
| `stale_after_slots` | u64 | staleness window |
| `bump` | u8 | PDA bump |

## Dependencies

- Inbound: `lib.rs` (entrypoints), consumers read `apy` + `is_stale`.
- Outbound: `constants`, `error`, `ed25519` (signature verification).

## Patterns & Gotchas

- **Version is bound into the signed message** — replaying an old signed payload
  is rejected by `validate_version`, not by signature alone.
- **`update_apy` is permissionless to relay** (anyone may submit) but the data
  must be signed by `publisher`; `ed25519.rs` enforces the binding.
- Staleness is a **read-side** concern: consumers call `is_stale` for the circuit
  breaker; `update_apy` itself does not block on staleness.
