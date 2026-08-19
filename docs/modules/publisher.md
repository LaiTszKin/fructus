# Module: Publisher (off-chain keeper)

**Purpose:** Feed the on-chain oracle as the *fallback* to user-driven pull —
fetch jitoSOL APY, sign, submit `ed25519 verify` + `update_apy`.

## Public API (TypeScript, `publisher/src/`)

| Export | File | Description |
| --- | --- | --- |
| `updateMessage` | `message.ts` | canonical sha256 (must equal on-chain `update_message`) |
| `updateApyDiscriminator` | `message.ts` | Anchor 8-byte `update_apy` discriminator |
| `fetchLatestApy` | `jito.ts` | `POST {JITO_API}/api/v1/stake_pool_stats` → latest decimal APY |
| `toScaledApy` | `jito.ts` | decimal → `u64` scaled, clamped to `[0, APY_SCALE]`, non-finite → 0 |
| `decodeOracle` | `state.ts` | read `apy/version/last_update_slot/stale_after_slots` from account data |
| `isStale` | `state.ts` | mirror of on-chain `is_stale` |
| `buildUpdateTx` / `submitUpdate` | `update.ts` | build+sign+submit the transaction |

## Data Flow

1. `index.ts` polls every `POLL_INTERVAL_MS`; reads current oracle state.
2. If scaled APY changed, `buildUpdateTx` signs the canonical message with the
   publisher keypair and adds an `Ed25519Program` instruction before `update_apy`.
3. `submitUpdate` sends it; a concurrent duplicate version is rejected on-chain.

## Dependencies

- Outbound: `@solana/web3.js`, Jito API (HTTP), Solana RPC.
- Cross-language contract: `updateMessage` == `programs/fructus/src/state.rs::update_message`.

## Patterns & Gotchas

- **`Keypair.sign(message)`** produces the 64-byte ed25519 signature; the
  `Ed25519Program.createInstructionWithPublicKey` instruction must precede
  `update_apy` (the program scans the whole transaction).
- `toScaledApy` clamps and handles `NaN`/`±Infinity` so a bad Jito response can
  never produce an out-of-bounds `u64`.
- `dist/` is build output (gitignored); run via `tsx`, not compiled JS.
