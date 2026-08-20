# Conventions

Rules that differ from or extend language defaults — things an agent cannot
reliably infer from config files alone.

## Solana "Address" migration

- **Never** depend on the `solana-program` umbrella crate in the program. Use the
  granular crates (`solana-sdk-ids`, `solana-instructions-sysvar`, `sha2`).
- Compare pubkeys/addresses **at byte level** (`as_ref()` / `to_bytes()`), never
  by type equality — anchor-lang 1.x's `Pubkey` and the 3.x granular crates'
  `Address` are different types.

## Fixed-point math

- APY and yield are `u64` scaled by `1_000_000` (`APY_SCALE`): `1.0 == 1_000_000`.
- Funding parameters (`funding_k`, `max_funding`) use the same `APY_SCALE`;
  margin fields are `u16` basis points (`≤ 10_000`, maintenance ≤ initial).
- Yield math uses `u128` intermediates + `checked_*`; staleness uses
  `saturating_sub` — no panicking arithmetic.

## Zero-copy accounts

Large accounts (the `OrderBook` is ~21 KB) **must** use `#[account(zero_copy)]`:
borsh deserialization copies the whole struct onto the SBF 4 KiB stack and fails
to build for accounts above it.

- Access via `AccountLoader::load()` / `load_mut()` / `load_init()` — never
  `Account::try_from` + `.exit()` (zero-copy writes in place; Anchor's
  `AccountsExit` writes the 8-byte discriminator after the handler returns).
- Sub-structs use `#[zero_copy]` (adds `#[repr(C)]` + `Copy/Clone/Pod/Zeroable`).
  `bytemuck::Pod` forbids implicit padding, so **reorder fields and add explicit
  `_pad: [u8; N]`** to make the layout packing-free.
- `bool` is not `Pod` (invalid bit patterns) → use `u8` (`0`/`1`).
- `u128` is avoided in zero-copy layouts (cross-target alignment) → store
  `[u8; 16]` and convert with `u128::from_le_bytes` / `to_le_bytes`.
- The `init` constraint needs `space = 8 + T::LEN` where `T::LEN = size_of::<T>()`
  (discriminator + payload); `#[derive(Default)]` is unreliable over `#[zero_copy]`,
  so implement `Default` manually.

## Canonical signature message

- Publisher signs `sha256("fructus::update_apy" ‖ oracle_addr ‖ apy_le(8) ‖ version_le(8))`.
- The Rust `update_message` and TypeScript `updateMessage` must stay byte-identical.
  Any change must update the cross-language vector test on **both** sides.

## Error handling

- On-chain: return `FructusError` variants via `require!` / `Err(..into())`.
- `read_*` helpers return `Option<T>` (not `Result`) for "malformed → absent".

## Testing

- Pure logic → `proptest` invariants in `programs/fructus/src/tests.rs`.
- Signature verification → mock instruction sysvar (`construct_instructions_data`).
- Cross-language consistency → shared hex vector asserted in Rust + TypeScript.
- On-chain stateful fuzzing → Trident (`trident-tests/`).

## Security rules

- Never commit `.env`, keypairs (`*.keypair.json`), or `target/` / `dist/` /
  `.review/` (all gitignored).
- `trident-tests/fuzz_0/{types.rs,fuzz_accounts.rs}` are generated — edit only
  `test_fuzz.rs`.

## Inferred vs documented

Any architectural rationale not written down in ADRs/comments is marked
`[INFERRED]` in docs — never present inference as fact.
