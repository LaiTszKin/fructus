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

- **Language:** Rust
- **Chain:** Solana
- **Framework:** Anchor
- **Key dependencies:** `anchor-lang`, `anchor-spl`, `solana-program`

## Repository layout

```
.
├── Anchor.toml                 # Anchor workspace configuration
├── Cargo.toml                  # Rust workspace manifest
├── programs/
│   └── fructus/                # On-chain program (smart contract)
│       ├── Cargo.toml
│       └── src/lib.rs
├── CHANGELOG.md
└── LICENSE
```

## Getting started

> **Prerequisites:** Rust toolchain, Solana CLI, and Anchor CLI. See
> [Anchor installation](https://www.anchor-lang.com/docs/installation).

```bash
# Build the workspace
anchor build

# Run unit tests
anchor test

# Deploy to a local validator
anchor localnet
```

> **Note:** the program keypair is generated locally at
> `target/deploy/fructus-keypair.json` (gitignored). Keep it safe — it is
> required to deploy and upgrade the program.

## Status

Fructus is in **early development** and is currently a **private** repository.
The protocol has not been audited and is not deployed to mainnet. Do not use it
with real funds.

## License

This project is licensed under the [MIT License](LICENSE).
