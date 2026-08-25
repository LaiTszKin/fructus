# Fructus — Solana yield-futures protocol (on-chain order book + collateral vault)

## Build & Test

- `cargo test --workspace` — full Rust program suite (200 lib tests: oracle/CLOB/vault/
  positions + funding/liquidation/settlement invariants, plus bank-style CPI tests)
- `cargo test --workspace <name>` — single test (e.g. `funding`, `liquidatable`)
- `anchor build` — compile program to `.so` (needs `cargo-build-sbf`); rebuild before
  running CPI tests — `tests/collateral_cpi.rs` loads the SBF binary and a stale
  `.so` fails the `cpi_binary_is_present_and_fresh` guard
- `cd publisher && npm test` — publisher suite (8 tests, cross-language vector)
- `cd sdk && npm test` — trader SDK suite (52 tests; funding/PnL/layout vector)
- `cd cli && npm test` — trader CLI suite (16 smoke + R-1 regression)
- `cd scripts && npm run e2e` — offline devnet lifecyle dry-run (`RUN_E2E=1` = live)
- `cd trident-tests && cargo run --bin fuzz_0` — on-chain stateful fuzz smoke run
- `cargo fmt --check` — format check

## Tech Stack

- **Language**: Rust (MSRV 1.89) + TypeScript (ESM, Node ≥ 18)
- **Framework**: anchor-lang 1.1.2 / anchor-spl 1.1.2
- **Solana crates**: `solana-sdk-ids` 3.1, `solana-instructions-sysvar` 3.0, `sha2` 0.11,
  `bytemuck` 1.17 (zero-copy accounts)
- **npm deps (publisher/sdk/cli/scripts)**: `@solana/web3.js` ^1.95, `tsx`
- **Testing**: `proptest` 1, `solana-instruction` 3.0 (dev), `solana-program-test` 3.1 (dev), Trident 0.12
- **Package managers**: cargo (root + `trident-tests/`) and npm (`publisher/`, `sdk/`, `cli/`, `scripts/`)

## Project Structure

- `programs/fructus/src/` — on-chain Anchor program: oracle (`state`, `ed25519`),
  settlement (`exchange`), CLOB order book + mark/twap (`orderbook`), collateral vault
  (`collateral`), position lifecycle (`positions`), **funding engine (`funding`)**,
  **liquidation engine (`liquidation`)**, top-level instructions (`lib`), pure-logic
  invariants (`tests`) + adversarial review invariants (`review_tests`)
- `programs/fructus/tests/` — bank-style CPI integration tests (`collateral_cpi.rs`, `positions_cpi.rs`)
- `publisher/` — off-chain TypeScript APY keeper (fetch → sign → submit)
- `sdk/` — trader TypeScript SDK (instruction builders, typed account decoders, funding/PnL mirrors)
- `cli/` — trader CLI over the SDK (open/close/deposit/withdraw/position/funding/mark/index)
- `scripts/` — devnet deploy + e2e lifecycle (`deploy.sh`, `e2e.mts`), `Anchor.toml` devnet profile
- `trident-tests/` — fuzz harness (separate cargo workspace)
- `docs/` — documentation hub ([docs/README.md](docs/README.md))
- `target/`, `*/node_modules`, `*/dist/`, `.review/` — build/review artifacts (gitignored)

## Key Constraints

- **Never depend on the `solana-program` umbrella crate** — use granular 3.x crates.
  Compare pubkeys at byte level (`as_ref()` / `to_bytes()`), not by type (anchor 1.x
  "Address" migration makes the types version-fragile).
- **Fixed-point APY/yield scale is `1_000_000`** (`APY_SCALE`); use `u128` + `checked_*`
  / `saturating_*` arithmetic — no panicking math. **Funding / premium / realized PnL are
  signed half the time: use `i128` + `checked_*`/`saturating_*`**, never `u128`/`saturating`.
- **Funding sign convention**: `premium = mark − index`; `funding_rate =
  clamp(funding_k·premium/APY_SCALE, ±max_funding)`; `premium > 0 ⇒ **longs pay shorts**`
  (long flow `−1`, short flow `+1`, exact opposites). Epoch = `slot / funding_epoch_slots`;
  settlement is idempotent (same epoch ⇒ no-op).
- **Canonical signed message** = `sha256("fructus::update_apy" ‖ oracle ‖ apy_le ‖ version_le)`.
  Rust `update_message` and TS `updateMessage` must stay byte-identical; any change
  updates the cross-language vector test on both sides.
- **Cross-language funding/PnL mirrors** — `sdk/src/{funding,positions,mark-index}.ts`
  and `cli` must stay byte-identical to Rust `funding.rs`/`positions.rs`/`orderbook.rs`
  (sign, clamp, truncate-toward-zero, annualize, `mid().unwrap_or(index)` fallback).
- **`trident-tests/fuzz_0/{types.rs,fuzz_accounts.rs}` are generated** — edit only
  `test_fuzz.rs`.
- Stake-pool offsets are 258/266 (with `account_type` prefix) — do not "fix" to 257/265.
- **Large accounts (> 4 KiB) must be `#[account(zero_copy)]`** — borsh deserialization
  overflows the SBF 4 KiB stack. Access via `AccountLoader::load_mut()`/`load_init()`
  (no `.exit()`); sub-structs use `#[zero_copy]` with `#[repr(C)]`, reordered fields +
  explicit `_pad` (bytemuck `Pod` forbids implicit padding); `bool` → `u8`, `u128` →
  `[u8; 16]`.
- **Devnet deploy** — `scripts/deploy.sh` builds + deploys and records the program id /
  `PerpMarket` PDA; align `[programs.devnet]` with `declare_id!` (PDA derivation depends
  on the program id). A real deploy needs the program keypair + a funded devnet wallet.

## Testing

- Pure logic → `proptest` invariants in `programs/fructus/src/tests.rs` and the per-module
  `#[cfg(test)]` (funding/liquidation/positions), plus `review_tests.rs` (adversarial).
- Signature verification → mock instruction sysvar (`construct_instructions_data`).
- Cross-language consistency → shared hex vector (Rust + TS) + SDK/cli vector tests.
- Stateful on-chain → Trident `trident-tests/`.
- Vault CPI / bank-style → `solana-program-test` in `programs/fructus/tests/` (needs a
  freshly built `.so`).

## Git Workflow

- Conventional commits: `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`.
- Commit in dependency order: `docs:`/`chore:` → `refactor:` → `feat:`/`fix:` → `test:`.

## Documentation

- `docs/` — architecture, modules, API, data models, setup, testing, workflows
  ([docs/README.md](docs/README.md)).
- Keep "one home per fact": root `README.md` links in; it does not duplicate deep content.
- Mark inferred rationale `[INFERRED]` — never present inference as fact.

## Boundaries

**Always:**

- Run `cargo test --workspace` before committing program changes (and `anchor build` so
  the CPI guard stays green).
- Add/adjust property tests for any changed pure logic (`proptest`).
- Keep the cross-language message vector (oracle) and the funding/PnL mirrors in sync
  across Rust + TypeScript.

**Ask first:**

- Adding new Solana/Anchor dependencies (version-sensitivity is high).
- Changing the stake-pool offsets or the canonical message format.
- Deploying/upgrading the program or rotating the publisher key.

**Never:**

- Commit `.env`, keypairs (`*.keypair.json`), `target/`, `dist/`, `.review/`, or secrets.
- Edit generated files (`trident-tests/fuzz_0/types.rs`, `fuzz_accounts.rs`).
- Skip pre-commit hooks with `--no-verify` without explicit request.
