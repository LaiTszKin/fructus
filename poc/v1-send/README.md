# `poc/v1-send` — Transaction V1 (SIMD-0385) send PoC

Issue **#18** of the Stage-2 plan. A self-contained, private npm package that drives exactly **one
system transfer** through a **Transaction V1** message on devnet — the smallest walk that proves the
v1 send path (kit 8.3 message construction → one simulation for the resource limits → compile →
sign → base64 send → confirm) really works against a v1-active cluster, and that pins the pitfalls
#19 must respect.

It is deliberately **not** part of any workspace and **not** wired into CI:

- the cargo workspace is `members = ["programs/*"]` (root `Cargo.toml`), so `poc/` is outside it;
- the CI `ts` matrix names `publisher · sdk · cli · scripts` only (`.github/workflows/ci.yml`), so
  no job installs or runs this package;
- `@solana/kit` is a dependency **only** here — the five shipped packages are untouched.

Nothing in this directory writes a secret: the keypair is read from `KEYPAIR` and never copied,
printed, or committed.

## Requirements

- Node ≥ 22 (measured on v22.22.3), npm 10.x.
- A **funded devnet keypair**. The walk's step 4 simulates the transaction, and a simulation cannot
  succeed while the fee payer has no lamports, so an unfunded keypair stops the run at step 4 with
  exit code 3. Fund it with `solana airdrop 2 <pubkey> --url devnet` (the public faucet is often
  rate-limited: *"You've either reached your airdrop limit today or the airdrop faucet has run
  dry"*) or via <https://faucet.solana.com>.

Dependency versions are pinned in `package.json`; `npm install` resolves exactly: `@solana/kit`
8.3.0, `@solana/web3.js` 1.99.0, `tsx` 4.23.15, `typescript` 7.0.2, `@types/node` 24.13.6.

## Environment variables

| Variable | Default | Meaning |
| --- | --- | --- |
| `KEYPAIR` | `~/.config/solana/id.json` | Path to the funded signer (a Solana CLI keypair JSON file). |
| `RPC_URL` | `https://api.devnet.solana.com` | Cluster RPC. Must be a cluster where the v1 feature account is active. |
| `AMOUNT_LAMPORTS` | `1` | Lamports for the self-transfer. |
| `DRY_RUN` | unset | `DRY_RUN=1` (or `--dry-run`): the full walk minus `sendTransaction`. Step 4 still simulates, so the fee payer must still be funded. |
| `BUILD_ONLY` | unset | `BUILD_ONLY=1` (or `--build-only`): steps 1–3 plus the pitfall probes — no simulation, no send. Runs on an unfunded keypair. |

## Run

```sh
cd poc/v1-send && npm install && npm run poc
```

Useful variants:

```sh
DRY_RUN=1 npm run poc            # build, estimate, sign — never send
npm run poc -- --build-only      # rpc-free-of-funds: gate check + build + probes only
KEYPAIR=~/.config/solana/other.json RPC_URL=https://api.devnet.solana.com npm run poc
```

## The walk

1. **Keypair + RPC** — `KEYPAIR`, `RPC_URL`; prints the pubkey, balance and whether the account
   exists.
2. **Gate check first** — `getAccountInfo` on the devnet-active SIMD-0385 feature account
   `txv1aq4pp281K9um3tnPgkfX8UqtFT6wcVW3hNezGLL` (43 chars; the plan's original 41-character
   literal is invalid and the PoC prints that rejection as evidence). Absent ⇒ exit **2**, nothing is
   built or sent (fail closed).
3. **Build** — `createTransactionMessage({ version: 1 })` + fee payer + blockhash lifetime + **one**
   `SystemProgram.transfer` whose recipient is the sender. The web3 instruction goes through
   `toKitInstruction()` (programId/keys/data + signer/writable flags → programAddress/accounts/data
   + `AccountRole`) — the same bridge #19 needs.
4. **ONE simulation, then page rounding** — kit's `estimateResourceLimitsFactory` maxes compute units
   (1,400,000) and loaded data (64 MiB) and simulates once; the returned
   `loadedAccountsDataSizeLimit` is rounded **up to the next 32 KiB page** and both values are
   written back with `setTransactionMessageConfig`. The 32 KiB rounding lives here (caller side), not
   in the kit estimator.
5. **Compile → sign → send → confirm** — `compileTransaction` → `createKeyPairSignerFromBytes` over
   the web3 `secretKey` → `signTransactionWithSigners` → base64 `sendTransaction` → web3
   `confirmTransaction`; prints the confirmed signature and the explorer URL.
6. **Pitfall probes** — two extra simulations that validate the pitfalls #19 encodes, plus the
   pitfalls themselves printed with the values this run measured.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | The walk completed (or the intentional `--build-only` / `DRY_RUN` run did). |
| `2` | The v1 gate is inactive — refused before building anything. |
| `3` | The fee payer is unfunded / the send leg is blocked. |
| `1` | Any other failure. |

## Pitfalls this PoC pins

- Unset v1 resource limits are **zero**, never defaults: the config mask carries exactly what was
  set, and a v1 message that leaves them unset budgets 0 compute units.
- The priority fee is a **total in lamports** (`priorityFeeLamports`), not a price per compute unit.
- **base64** is mandatory above 1232 bytes; kit's `sendTransaction` only ever encodes base64.
- **ComputeBudget instructions are no-ops** under v1 — never include them (probe A).
- v1 has **no ALT support**: the compiled v1 message carries `staticAccounts` only, with no
  `addressTableLookups` field.
- **Duplicate addresses are rejected**: kit merges repeated references into one static account
  (fee payer == sender == recipient compiles to two addresses), and a hand-spliced duplicate is
  refused by the node (probe B: *"invalid transaction: Transaction failed to sanitize accounts
  offsets correctly"*).
