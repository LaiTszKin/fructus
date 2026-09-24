# Setup

## Prerequisites

- Rust toolchain ≥ 1.89 (`rustup`)
- Anchor CLI 1.2.0 (via `avm`)
- `cargo-build-sbf` 4.x (BPF toolchain — required for `anchor build` / Trident)
- `cargo-nextest` 0.9 (`cargo install cargo-nextest --locked`) — the default test runner
- Node.js ≥ 18 + npm (for the publisher)
- Trident CLI 0.12 (`cargo install trident-cli --locked`) — for fuzzing

## Install

```bash
git clone https://github.com/LaiTszKin/fructus.git
cd fructus

# Rust workspace (program)
cargo build --workspace

# Publisher
cd publisher && npm install && cd ..

# Fuzz harness (separate workspace)
cd trident-tests && cargo build && cd ..
```

## Git hooks

One line per clone (hooks are not cloned automatically):

```bash
git config core.hooksPath .githooks
```

`.githooks/pre-commit` runs `cargo fmt --all` on every commit and re-stages the Rust
files that belong to the commit, so a commit can never land unformatted. Skip it once
with `git commit --no-verify`. CI enforces the same via `cargo fmt --check`.

## Environment (publisher)

Copy `publisher/.env.example` → `.env` and fill:

| Var | Description |
| --- | --- |
| `RPC_URL` | Solana RPC endpoint |
| `PUBLISHER_KEYPAIR` | JSON byte-array secret key of the publisher |
| `PROGRAM_ID` | `8ZLiJ12eBiam4UP2HRp3M75CQAcc8GuUBz44zeHt6mjH` |
| `ORACLE_ADDRESS` | the `yield_oracle` PDA |
| `JITO_API` | `https://kobe.mainnet.jito.network` |
| `POLL_INTERVAL_MS` | poll interval (default 3600000) |

## Run

```bash
# On-chain: build to .so (for deploy/test)
anchor build

# Publisher: one-shot poll loop
cd publisher && npm run publish

# Fuzz: run the fuzz target
cd trident-tests && cargo run --bin fuzz_0
```

## Build profiles

Local `dev`/`test` builds carry **no debug info** (`Cargo.toml` →
`[profile.dev] debug = false`): DWARF dominates both the link step and `target/`,
and the Solana test runtime pulls in a lot of dependencies. Backtraces keep their
symbol names but lose file/line numbers — for a session that needs line-level
debugging, override it per run:

```bash
CARGO_PROFILE_DEV_DEBUG=2 cargo test --workspace --lib
```

The on-chain `.so` is built by `cargo-build-sbf` from the `release` profile, which
states `debug = false` explicitly too.

## Verify

```bash
cargo nextest run --workspace  # Rust suite (fallback: `cargo test`, doctests only)
cd publisher && npm test       # 8 publisher tests
```
