# Testing

## Frameworks

| Layer | Tool | Location |
| --- | --- | --- |
| Property-based (pure logic) | `proptest` | `programs/fructus/src/tests.rs` |
| Signature verification (e2e) | mock instruction sysvar | `programs/fructus/src/tests.rs` |
| Cross-language vector | `node:test` | `publisher/test/message.test.ts` |
| On-chain stateful fuzz | Trident | `trident-tests/fuzz_0/test_fuzz.rs` |

## Commands

```bash
cargo test --workspace                    # full Rust suite (31 tests)
cargo test --workspace <test_name>        # single test
cd publisher && npm test                  # TS suite (8 tests)
cd trident-tests && cargo run --bin fuzz_0   # fuzz (1000 iters × 100 flows)
```

## Invariants (property tests)

- Staleness predicate equals `cur.saturating_sub(last) >= window`; monotonic in `cur`.
- Version strictly increases; replay rejected.
- APY within `[0, 1_000_000]`.
- Canonical message deterministic + input-sensitive; matches a fixed hex vector.
- `ExchangeRate::read` round-trips; rejects zero supply / wrong discriminator.
- `realized_yield` self-yield == 0; monotonic in settle numerator.
- `annualize` identity when period == year; rejects zero period.
- `PerpMarket` init bounds: `funding_k` ∈ [1, 1_000_000]; `max_funding` ≤ 1_000_000;
  `initial_margin_bps` ∈ (0, 10_000]; `maintenance_margin_bps` ∈ (0, initial] —
  asserted as exact interval equivalence plus boundary edges.

## Signature verification (mock sysvar)

`verify_publisher_signature` is exercised end-to-end against a real serialized
instruction list (no validator needed): matching publisher accepted; wrong
publisher / wrong message / missing instruction rejected; unrelated ed25519
instructions skipped.

## Cross-language lock

The publisher's `updateMessage` and the program's `update_message` must produce
the same sha256. Vector: oracle `0x01×32`, apy `71840`, version `1` →
`dd9394a5f5b4b383f2478ae97164cb69b495245a220a1be1d0996a0e0d54c1a0`
(asserted in both Rust and TypeScript).

## Mock policy

- On-chain: no external services — the mock instruction sysvar replaces the runtime.
- Publisher: no mocks; `toScaledApy`/`isStale`/`decodeOracle` are pure and unit-tested.
- Fuzz: uses `TridentSVM` (in-process), signing real ed25519 payloads with a fixed keypair.
