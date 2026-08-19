# Fructus yield-oracle publisher

Off-chain keeper that feeds the Fructus mark-price APY oracle.

## Role

The protocol's normal path is **user-driven pull** (a user's transaction carries
the signed APY update). This keeper is the **fallback** that keeps the on-chain
APY fresh for third-party readers and low-liquidity windows — it only writes
when the value actually moved (change detection), and is permissionless to run
because the data is signed by the publisher key.

## How it works

1. Fetch the latest jitoSOL APY from Jito's `stake_pool_stats` endpoint.
2. Read the current on-chain oracle `version`.
3. If the scaled APY changed, sign the canonical message (SHA-256, matching the
   on-chain `update_message`) with the publisher keypair.
4. Submit a transaction: `ed25519 verify` instruction + `update_apy`.

## Setup

```bash
npm install
cp .env.example .env   # fill in RPC_URL, PUBLISHER_KEYPAIR, ORACLE_ADDRESS, …
```

## Run

```bash
npm run publish        # one-shot poll loop (runs every POLL_INTERVAL_MS)
npm test               # cross-language message-vector test
```

## Cross-language consistency

`src/message.ts::updateMessage` must equal the on-chain
`programs/fructus/src/state.rs::update_message`. Both are locked to the vector:

- oracle = `0x01 × 32`, apy = `71840`, version = `1`
- SHA-256 = `dd9394a5f5b4b383f2478ae97164cb69b495245a220a1be1d0996a0e0d54c1a0`

(asserted in `programs/fructus/src/tests.rs` and `test/message.test.ts`).
