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
- Yield math uses `u128` intermediates + `checked_*`; staleness uses
  `saturating_sub` — no panicking arithmetic.

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
