# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- On-chain order book (CLOB) + mark discovery (#3):
  - `OrderBook` zero-copy account (`#[account(zero_copy)]`, ~21 KB) holding the
    full bid/ask book, a bounded 128-entry event queue, and a 16-sample TWAP
    ring inline (no per-order PDAs).
  - Pure matching engine (`orderbook.rs`): price-time priority, partial fills,
    no self-trade, no over-fill, bounded by `MAX_MATCH_STEPS`, with a
    permissionless `crank` to drain the event queue and resume budget-interrupted
    takers.
  - Instructions: `initialize_order_book`, `place_limit_order`,
    `place_market_order`, `cancel_order`, `crank`.
  - `mark()` = book mid (`(best_bid + best_ask) / 2`) and `twap()` (time-weighted
    mid) derived on-chain; property tests for the matching/mark/twap invariants.
- Collateral vault + deposit/withdraw (#4):
  - USDC collateral-vault token account at the `PerpMarket.vault` PDA (self-
    authorized), created via `anchor-spl` CPI.
  - `UserCollateral` per-`(market, user)` ledger (`deposited` + `reserved`,
    `reserved` stubbed `0`) with a `free_collateral` seam for #5.
  - Instructions: `initialize_collateral_vault`, `deposit_collateral`,
    `withdraw_collateral`; bank-style CPI tests (solana-program-test) exercise
    real token-account movement.
- Perpetual market account (`PerpMarket`) + `initialize_market` instruction:
  - Singleton `PerpMarket` PDA (seed `"perp_market"`) binding the jitoSOL
    stake-pool `index_source`, USDC `collateral_mint`, funding params
    (`funding_k`, `max_funding`, `funding_epoch_slots`), margin params
    (`initial_margin_bps`, `maintenance_margin_bps`), `authority`, and the
    collateral `vault` PDA.
  - `initialize_market` validates `index_source` (SPL Stake Pool owner +
    discriminator), enforces fixed-point funding bounds and basis-point margin
    bounds, derives the vault PDA, and stores all fields atomically.
  - Property-based tests for the four validation bounds (proptest + boundary).
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
- Position lifecycle (open/close long & short) + maker settlement (#5):
  - Per-`(market, user, side)` `Position` ledger with notional-weighted entry
    running sums (`entry_n_sum`/`entry_d_sum`), ledger-only margin
    (`Position.collateral` ↔ `UserCollateral.reserved`), and reuse of the CLOB.
  - Instructions: `open_position` (taker-fulfill inline), `close_position`
    (lifecycle-only), `settle_fill` (idempotent maker settlement), `reset_position`.
  - Pure `positions.rs` module: `margin_required` (ceiling), entry accumulation,
    signed PnL (`signed_yield_change` + `pnl`), `apply_pnl` (credit/clamp);
    `proptest` invariants for margin bounds, leverage cap, and PnL sign.
- Funding engine (#6) — anchor on-chain `mark` to the trustless `index`:
  - Sign convention `premium > 0 ⇒ longs pay shorts` (exact opposites); signed
    `i128` `premium`/`funding_rate` (clamped `±max_funding`) /`funding_payment`;
    epoch = `slot / funding_epoch_slots`, idempotent per-epoch accrual.
  - `PerpMarket` gains `funding_epoch`/`index_n`/`index_d`/`funding_accumulator`;
    permissionless `settle_funding` (per-position), mark falls back to `index`
    on a one-sided/empty book.
- Trustless realized-yield settlement (#7):
  - `Position.closed_notional`; permissionless `settle_close` realizes the
    **signed** index-based `positions::pnl` into `UserCollateral.deposited`
    (negative clamped so the vault never goes negative); depends only on
    `exchange.rs` data, never the mark oracle.
- Liquidation engine (#8) — solvency backstop at the order-book TWAP:
  - `liquidation.rs`: `equity`, `maintenance_margin`, `liquidatable` (strict `<`,
    equality healthy, zero-notional never liquidatable), `liquidation_penalty`
    (bounded by collateral), `apply_liquidation` (partial/full, rewards the
    liquidator, never yields negative collateral).
  - Permissionless `liquidate` instruction (partial + full, penalty to the
    liquidator; i128 health math, no panicking arithmetic).
- Trader TypeScript SDK (#10):
  - `sdk/` — instruction builders for the full program surface, typed account
    decoders (`PerpMarket`/`Position`/`UserCollateral`/`OrderBook`/`YieldOracle`),
    funding/PnL/mark/index helpers, cross-language vector tests (byte-identical
    to the Rust `funding.rs`/`positions.rs`/`orderbook.rs`).
- Trader CLI (#11):
  - `cli/` — `open`/`close`/`deposit`/`withdraw`/`position`/`funding`/`mark`/
    `index` over the SDK: dry-run by default, `--network`/`--submit` live.
- Devnet deployment + end-to-end lifecycle (#9):
  - `scripts/` — `deploy.sh` (build + deploy + record program id / PerpMarket PDA),
    `e2e.mts` (deploy → init → deposit → open long/short → funding → close →
    settle, asserting the funding sign convention), `Anchor.toml` devnet profile.
- Adversarial review invariants (`review_tests.rs`, `sdk/test/review-invariants.test.ts`):
  property-based proof over the funding/liquidation/settlement logic; one
  confirmed CLI mark-fallback bug found and fixed (`cli/src/commands/funding.ts`).

### Changed

- Relocated the adversarial-review invariants out of a monolithic
  `programs/fructus/src/review_tests.rs` into the standard per-module
  `#[cfg(test)]` blocks (`funding.rs`/`liquidation.rs`/`positions.rs`/
  `collateral.rs`) and `src/tests.rs` (lib-adapter level), following the repo's
  per-module test convention. `review_tests.rs` is removed; `AGENTS.md` updated.

### Fixed

- `settle_close` no longer re-prices a prior closed amount at the newest
  generation's basis: `apply_close_fills` **accumulates** each closed
  generation's close-time entry basis into `closed_entry_*` as a
  notional-weighted harmonic mean (`positions::accumulate_closed_entry`), so a
  re-open between closes (before any `settle_close`) leaves each pending closed
  notional priced at its own close-time entry basis (R-S1/R-S2).

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
