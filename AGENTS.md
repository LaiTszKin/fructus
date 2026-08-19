# Fructus — Solana yield-futures protocol (data module)

## Build & Test

- `cargo test --workspace` — full Rust program suite (23 tests, property-based + mock-sysvar)
- `cargo test --workspace <name>` — single test
- `anchor build` — compile program to `.so` (needs `cargo-build-sbf`)
- `cd publisher && npm test` — publisher suite (8 tests, cross-language vector)
- `cd trident-tests && cargo run --bin fuzz_0` — on-chain stateful fuzz smoke run
- `cargo fmt --check` — format check

## Tech Stack

- **Language**: Rust (MSRV 1.89) + TypeScript (ESM, Node ≥ 18)
- **Framework**: anchor-lang 1.1.2 / anchor-spl 1.1.2
- **Solana crates**: `solana-sdk-ids` 3.1, `solana-instructions-sysvar` 3.0, `sha2` 0.11
- **Publisher deps**: `@solana/web3.js` ^1.95, `tsx`
- **Testing**: `proptest` 1, `solana-instruction` 3.0 (dev), Trident 0.12
- **Package managers**: cargo (root + `trident-tests/`) and npm (`publisher/`)

## Project Structure

- `programs/fructus/src/` — on-chain Anchor program (oracle, settlement, ed25519 verify)
- `publisher/` — off-chain TypeScript keeper (fetch → sign → submit)
- `trident-tests/` — fuzz harness (separate cargo workspace)
- `docs/` — documentation hub ([docs/README.md](docs/README.md))
- `target/`, `publisher/dist/`, `.review/` — build/review artifacts (gitignored)

## Key Constraints

- **Never depend on the `solana-program` umbrella crate** — use granular 3.x crates.
  Compare pubkeys at byte level (`as_ref()` / `to_bytes()`), not by type (anchor 1.x
  "Address" migration makes the types version-fragile).
- **Fixed-point APY/yield scale is `1_000_000`** (`APY_SCALE`); use `u128` + `checked_*`
  / `saturating_*` arithmetic — no panicking math.
- **Canonical signed message** = `sha256("fructus::update_apy" ‖ oracle ‖ apy_le ‖ version_le)`.
  Rust `update_message` and TS `updateMessage` must stay byte-identical; any change
  updates the cross-language vector test on both sides.
- **`trident-tests/fuzz_0/{types.rs,fuzz_accounts.rs}` are generated** — edit only
  `test_fuzz.rs`.
- Stake-pool offsets are 258/266 (with `account_type` prefix) — do not "fix" to 257/265.

## Testing

- Pure logic → `proptest` invariants in `programs/fructus/src/tests.rs`.
- Signature verification → mock instruction sysvar (`construct_instructions_data`).
- Cross-language consistency → shared hex vector (Rust + TS).
- Stateful on-chain → Trident `trident-tests/`.

## Git Workflow

- Conventional commits: `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`.
- Commit in dependency order: `docs:`/`chore:` → `refactor:` → `feat:`/`fix:` → `test:`.

## Documentation

- `docs/` — architecture, modules, API, data models, setup, testing, workflows.
- Keep "one home per fact": root `README.md` links in; it does not duplicate deep content.
- Mark inferred rationale `[INFERRED]` — never present inference as fact.

## Boundaries

**Always:**

- Run `cargo test --workspace` before committing program changes.
- Add/adjust property tests for any changed pure logic.
- Keep the cross-language message vector in sync across Rust + TypeScript.

**Ask first:**

- Adding new Solana/Anchor dependencies (version-sensitivity is high).
- Changing the stake-pool offsets or the canonical message format.
- Deploying/upgrading the program or rotating the publisher key.

**Never:**

- Commit `.env`, keypairs (`*.keypair.json`), `target/`, `dist/`, `.review/`, or secrets.
- Edit generated files (`trident-tests/fuzz_0/types.rs`, `fuzz_accounts.rs`).
- Skip pre-commit hooks with `--no-verify` without explicit request.
