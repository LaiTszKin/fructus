# Workflows

## Publish an APY update (keeper)

1. `publisher/src/index.ts` polls `JITO_API` for the latest APY.
2. `decodeOracle` reads the current on-chain version.
3. If `toScaledApy(newApy) != current.apy`, build `ed25519 verify` +
   `update_apy(apy, version+1)` and `submitUpdate`.
4. A duplicate/concurrent version is rejected on-chain by `validate_version`.

## Add an instruction to the program

1. Add handler + `#[derive(Accounts)]` context in `programs/fructus/src/lib.rs`.
2. Add error variants to `error.rs` if needed.
3. Add property tests to `tests.rs` (red → green).
4. Update `docs/api-reference.md` and the relevant `docs/modules/*.md`.
5. Run `cargo test --workspace`.

## Rotate the publisher key

1. Generate a new keypair; sign data with it going forward.
2. Call `set_publisher(new_pubkey)` as `authority`.
3. Update `publisher/.env` `PUBLISHER_KEYPAIR`.

## Run the fuzzer

1. `trident-tests` is a separate workspace; the program `.so` is at
   `target/deploy/fructus.so` (from `anchor build`).
2. Edit `trident-tests/fuzz_0/test_fuzz.rs` (only that file — `types.rs`/
   `fuzz_accounts.rs` are generated).
3. `cd trident-tests && cargo run --bin fuzz_0` (or `trident fuzz run fuzz_0` for a
   honggfuzz campaign).

## Settlement (future contract integration)

1. Snapshot `ExchangeRate` at contract open (via `read_exchange_rate` data).
2. At settle, read the rate again and compute `realized_yield` + `annualize`.
3. The settlement value never depends on the mark-price oracle.
