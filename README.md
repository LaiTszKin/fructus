# Fructus

> **Yield futures on Solana — democratizing access to staking-yield hedging.**

Fructus is a Solana-native protocol for trading **yield futures**. It turns the
variable yield of liquid-staking assets into a standalone, tradeable instrument,
so anyone can hedge or speculate on future yield **without understanding
interest-rate swaps**.

The protocol ships in stages:

1. **jitoSOL yield perpetual futures** (MVP)
2. **jitoSOL dated futures** (fixed-expiry contracts)
3. **Expansion to other yield-bearing assets**

## Documentation

Full project documentation lives under [`docs/`](docs/README.md) — architecture,
modules, instruction reference, data models, setup, testing, and workflows.
Agent orientation is in [`AGENTS.md`](AGENTS.md).

## Why Fructus?

Liquid staking (for example, jitoSOL) pays a variable yield. Today, separating
that yield from the underlying asset and trading it is something only
sophisticated desks do. Fructus makes the yield itself a first-class, liquid
product with two goals:

| Goal | What it enables |
| --- | --- |
| **Democratize investing** | Retail users hedge staking risk directly — *short* a yield future to lock in their staking return — with no swaps knowledge required. |
| **Better institutional tooling** | Funds and market makers *long* yield futures to harvest basis, or assemble structured products on top of them. |

## Core concepts

### Yield perpetual futures

A perpetual contract on an asset's **yield rate** rather than its price. A
short position locks in a fixed yield, while a long position speculates that
yield will rise above the current market level. Funding keeps the perp anchored
to the underlying staking yield.

### Yield dated futures

Fixed-expiry contracts that settle against the realized yield of a staking
asset over a defined period (for example, the annualized jitoSOL APY between
two dates). These are the building blocks for terms, calendar spreads, and
structured products.

## Use cases

- **Stakers (hedgers):** short yield futures to convert variable staking
  rewards into a fixed, predictable return.
- **Yield farmers:** hedge against APY compression on concentrated positions.
- **Basis traders:** long yield futures against staked exposure to earn the
  spread between implied and realized yield.
- **Structured-product issuers:** use dated yield futures as legos for
  principal-protected notes and yield-enhanced vaults.

## Technology

- **Language:** Rust (on-chain) + TypeScript (off-chain keeper)
- **Chain:** Solana
- **Framework:** Anchor 1.1
- **Key dependencies:** `anchor-lang`, `anchor-spl`, `solana-sdk-ids`, `@solana/web3.js`
- **Fuzzing:** Trident

## Repository layout

```
.
├── Anchor.toml                 # Anchor workspace configuration
├── Cargo.toml                  # Rust workspace manifest
├── AGENTS.md                   # Agent orientation (build/test/conventions)
├── docs/                       # Project documentation (docs/README.md hub)
├── programs/
│   └── fructus/                # On-chain program (oracle, settlement, ed25519)
│       └── src/
├── publisher/                  # Off-chain TypeScript keeper
├── trident-tests/              # On-chain fuzz harness
├── CHANGELOG.md
└── LICENSE
```

## Getting started

> **Prerequisites:** Rust toolchain, Anchor CLI, and `cargo-build-sbf`. See
> [Anchor installation](https://www.anchor-lang.com/docs/installation).

```bash
# Build the on-chain program
anchor build

# Run the on-chain test suite
cargo test --workspace

# Run the off-chain publisher tests
cd publisher && npm test

# Run the on-chain fuzz smoke test
cd trident-tests && cargo run --bin fuzz_0
```

> **Note:** the program keypair is generated locally at
> `target/deploy/fructus-keypair.json` (gitignored). Keep it safe — it is
> required to deploy and upgrade the program.

## Status

Fructus is in **early development** and is currently a **private** repository.
The **data module** (mark-price APY oracle, trustless settlement reference,
off-chain keeper, fuzz harness) is implemented and tested; the yield-futures
trading logic is next. The protocol has not been audited and is not deployed to
mainnet. Do not use it with real funds.

## License

This project is licensed under the [MIT License](LICENSE).
