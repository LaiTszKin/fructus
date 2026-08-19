# Setup

## Prerequisites

- Rust toolchain ≥ 1.89 (`rustup`)
- Anchor CLI 1.1.2 (via `avm`)
- `cargo-build-sbf` 4.x (BPF toolchain — required for `anchor build` / Trident)
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

## Verify

```bash
cargo test --workspace        # 23 program tests
cd publisher && npm test       # 8 publisher tests
```
