# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Yield oracle data module (mark-price APY):
  - `YieldOracle` singleton PDA state (`apy`, `version`, `last_update_slot`,
    `publisher`, `authority`, `stale_after_slots`).
  - Instructions: `initialize`, `update_apy` (publisher-signed ed25519),
    `set_stale_window`, `set_publisher` (authority-only).
  - Ed25519 signature verification via instruction introspection, with
    version monotonicity and APY bounds enforced.
  - Property-based tests (`proptest`) for staleness, version, bounds, message
    determinism, and ed25519 instruction parsing.
- Trustless settlement module (`exchange.rs`):
  - Reads the jitoSOL stake-pool exchange rate (`total_lamports /
    pool_token_supply`) and derives realized yield, with `proptest` coverage.
- Off-chain publisher (`publisher/`, TypeScript):
  - Fetches jitoSOL APY from Jito, signs the canonical message, and submits
    `ed25519 verify` + `update_apy`; cross-language message vector locked
    against the on-chain `update_message`.
- End-to-end ed25519 verification tests using a mock instruction sysvar
  (matching publisher accepted; wrong publisher/message/missing instruction
  rejected).
- Trident fuzz harness (`trident-tests/`): stateful fuzzing of `initialize` +
  publisher-signed `update_apy`, asserting APY bounds and version monotonicity.

## [0.1.0] - 2026-08-19

### Added

- Initial repository scaffolding for the Fructus protocol.
- Anchor/Rust workspace with a placeholder on-chain program (`programs/fructus`).
- Project documentation (`README.md`), MIT `LICENSE`, and this `CHANGELOG.md`.

## [0.1.0] - 2026-08-19

### Added

- First release. Establishes the project as a Solana yield-futures protocol
  targeting jitoSOL yield perpetual futures as the MVP, followed by jitoSOL
  dated futures and expansion to other yield-bearing assets.

[Unreleased]: https://github.com/LaiTszKin/fructus/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/LaiTszKin/fructus/releases/tag/v0.1.0
