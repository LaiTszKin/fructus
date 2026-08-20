# Tech Stack

| Component | Version | Purpose | Notes |
| --- | --- | --- | --- |
| **Runtime / Language** | | | |
| Rust | 1.89+ (MSRV) | On-chain program | `edition = "2021"` |
| Node.js | ≥ 18 | Off-chain publisher | Dev on v26 |
| TypeScript | ^5.5 | Off-chain publisher | ESM, `moduleResolution: NodeNext` |
| **Solana / Anchor** | | | |
| anchor-lang | 1.1.2 | Program framework | workspace dep |
| anchor-spl | 1.1.2 | SPL helpers | workspace dep |
| bytemuck | 1.17 | zero-copy account layouts | `derive` + `min_const_generics` |
| solana-sdk-ids | 3.1.0 | Well-known program IDs | `ed25519_program`, sysvars |
| solana-instructions-sysvar | 3.0.0 | Instruction introspection | for ed25519 verify |
| sha2 | 0.11 | SHA-256 canonical message | mirrors off-chain signing |
| **Publisher (npm)** | | | |
| @solana/web3.js | ^1.95 | Transactions, Keypair, Ed25519Program | |
| tsx | ^4.15 | TypeScript runner | `npm test`, `npm run publish` |
| @types/node | ^20 | Node typings | |
| **Testing / Fuzzing** | | | |
| proptest | 1 | Property-based tests (Rust) | dev-dep |
| solana-instruction | 3.0.0 | Mock instruction sysvar in tests | dev-dep |
| solana-program-test | 3.1 | Bank-style CPI integration tests | dev-dep (loads the SBF `.so`) |
| Trident (trident-cli / trident-fuzz) | 0.12.0 | On-chain stateful fuzzing | `trident-tests/` |
| borsh | 1.5.3 | IDL/account (de)serialization in fuzz harness | |
| node:test | built-in | Publisher unit tests | |

## Package Managers

- **Rust**: `cargo` (workspace at repo root; members `programs/*`)
- **Publisher**: `npm` (separate `publisher/` project)
- **Fuzz**: `cargo` (separate `trident-tests/` workspace)

## Key Versions Note

anchor-lang 1.x sits on Solana's "Address" migration. The program avoids the
`solana-program` umbrella crate and depends on the granular `solana-sdk-ids` /
`solana-instructions-sysvar` crates instead; all pubkey comparisons are done at
byte level (`as_ref` / `to_bytes`) to stay version-agnostic. See
[conventions.md](conventions.md).
