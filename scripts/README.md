# Fructus — Devnet deploy + end-to-end lifecycle walk (issue #9)

This directory implements issue #9 (R-D1a…R-D1e): deploy the Fructus program to
devnet and run a scripted **end-to-end lifecycle test** that verifies the funding
sign convention (`premium > 0 ⟹ long pays, short receives`; R-F3).

Everything here is **best-effort and safe offline**. The network run is behind a
run flag (`RUN_E2E=1` / `npm run e2e:network`); the default `npm run e2e` derives
the PDAs and asserts the funding sign convention on a pure TypeScript mirror of
the funding engine — no RPC, no transaction, no deployment.

> ## TODO — fill in after a real devnet deploy
>
> The on-chain program id and the derived devnet `PerpMarket` address are
> recorded **after** a real `anchor deploy` (they are not known from here and
> cannot be pre-computed until lib.rs `declare_id!` matches the deployed id).
>
> 1. **Anchor.toml → `[programs.devnet] → fructus`** — set to the deployed devnet
>    program id (currently the `J2xccRtuG43drESLYznHhLhQkLTdfepcKYbiQ9BsJVaf`
>    placeholder, a valid-base58 id chosen so `anchor build` still parses).
> 2. **`programs/fructus/src/lib.rs` → `declare_id!`** — must equal that same id,
>    then `anchor build` + `anchor deploy` again. (The PDAs derive from the
>    program id, so a mismatch breaks all PDA derivation.)
> 3. **`PROGRAM_ID` / `MARKET` env** — after `bash scripts/deploy.sh`, copy the
>    printed ids into the env vars below.
>
> Until those are set, `npm run e2e` uses the default program id from
> `[programs.localnet]` for its offline PDA derivation (which is correct only for
> localnet; for a devnet check, set `PROGRAM_ID`).

---

## Files

| File | Purpose |
| --- | --- |
| `deploy.sh` (R-D1b) | `anchor build` + `anchor deploy` to devnet; prints/records the deployed program id + the derived `PerpMarket` (seed `"perp_market"`) / `OrderBook` / vault PDAs. |
| `e2e.mts` (R-D1c) | The end-to-end lifecycle walk (TS/ESM, runnable with `tsx`). Network calls are guarded behind `RUN_E2E=1`. |
| `package.json` (R-D1e) | `e2e`, `e2e:network`, `deploy`, `typecheck`, `build` scripts; deps mirror `publisher/` (`tsx`, `@solana/web3.js`). |
| `tsconfig.json` (R-D1e) | Strict ESM TypeScript config. |

> **Note on `e2e.mts` (not `.mjs`):** `tsx` does not strip TypeScript annotations
> from `.mjs` files, so a runnable **and strictly typechecked** walk must use a
> TypeScript module extension (`.mts`). All npm scripts reference the `.mts`
> path. If `.mjs` is strictly required, the module can be renamed and the
> tsconfig can use `allowJs`/`checkJs` instead.

---

## Prerequisites

- `anchor` + `cargo-build-sbf` on `PATH` (for `deploy.sh`).
- `node` ≥ 18.
- A devnet-funded keypair **in a file you never commit** (wallet/deploy/authority
  and the two trader keypairs).
- Deployed program id (see the TODO above).

---

## Install

```bash
cd scripts
npm install
```

### Optional tool: self-owned stake-pool bootstrap

The self-contained bootstrap (`npm run setup`) also creates a devnet SPL Stake
Pool to use as `INDEX_SOURCE`. That needs the stake-pool CLI (not shipped with
`spl-token`):

```bash
cargo install spl-stake-pool-cli
export PATH="$HOME/.cargo/bin:$PATH"
```

---

## Self-contained bootstrap (`npm run setup`)

The e2e walk needs three external inputs: devnet SOL (rent + fees), an index
source (a devnet SPL Stake Pool), and collateral (USDC). The external USDC
faucet and the lack of a usable devnet stake pool make those flaky, so
`scripts/selfcontained.mts` bootstraps them without either:

```bash
cd scripts
npm run setup            # wallets -> airdrop SOL -> collateral mint -> stake pool -> validate -> env
npm run setup -- --preflight   # tool + keypair check only; no transactions
```

What it does (idempotent — each step skips if already done):

1. **wallets**   — ensure `authority`, `long`, `short` keypairs (all `*.keypair.json`
   under `scripts/`, gitignored).
2. **sol airdrop** — funds the three wallets with devnet SOL (target: 2 / 0.1 / 0.1).
   Devnet airdrops are rate-limited per IP; if the faucet is saturated the script
   just logs the current balance and you re-run later (or airdrop from another
   connection).
3. **collateral** — creates a self-owned **6-decimal** SPL mint and mints the
   deposit into both traders' ATAs (no USDC faucet; see the security note below).
4. **stake pool** — `spl-stake-pool create-pool` + `deposit-sol` to give the pool a
   real (non-zero) exchange rate → the `INDEX_SOURCE`. Prints the pool address.
5. **validate** — asserts the pool satisfies Fructus `read_exchange_rate` (owner
   `SPoo1Ku8…`, `account_type` byte `1`, `total_lamports@258 > 0`,
   `pool_token_supply@266 > 0`) and that both trader ATAs hold ≥ `DEPOSIT_AMOUNT`.
6. **env** — prints the export block for `npm run deploy` + `npm run e2e:network`.

> **Collateral-mint security note (mainnet):** the on-chain program only ever
> checks that the collateral mint has `decimals == 6` (`lib.rs`
> `initialize_collateral_vault`); it does **not** require the mint to be USDC.
> So a self-minted test token is a *valid* collateral on devnet. That safety is
> a **trust-the-operator** property: on mainnet the deploy authority must choose
> a mint with no arbitrary mint authority (e.g. Circle USDC) — otherwise a
> mintable collateral token lets the operator inflate/settle against a rigged
> asset. This is a governance/parameter risk, **not** a program bug; the vault
> authority is the vault PDA and withdrawals are per-user, so no admin can
> directly drain the vault. See `docs/modules/liquidation.md`.

---

## Command sequence

### 1. Offline check (builds + typechecks, no network)

```bash
cd scripts
npm run typecheck      # tsc -p tsconfig.json --noEmit  (strict ESM)
npm run e2e            # offline dry-run: PDA derivation + R-F3 sign-convention assert
```

Expected offline output (excerpt):

```
[dry-run] PROGRAM_ID: 8ZLiJ12eBiam4UP2HRp3M75CQAcc8GuUBz44zeHt6mjH
[dry-run] perp_market PDA : EgMTaHdHmz6Z6BTe1DgX5sC63kqk5dXuVFhkWELumEPX (bump 252)
[dry-run] order_book PDA  : <order_book PDA> (bump <n>)
[dry-run] vault PDA       : <vault PDA> (bump <n>)

[outline] planned devnet lifecycle walk:
  1. initialize_market(index_source, usdc_mint, funding_k, max_funding, epoch_slots, im, mm)
  ...

[sign] R-F3 convention OK: premium>0 => long pays, short receives (exact opposites)

[dry-run] offline check OK. Set RUN_E2E=1 (or pass --network) plus the docs env to run it.
```

### 2. Deploy to devnet (R-D1b)

```bash
cd scripts
export DEVNET_WALLET="$HOME/.config/solana/devnet.json"   # deploy/authority keypair
export DEVNET_CLUSTER=devnet
export PROGRAM_ID="<deployed-devnet-id>"                   # or read from Anchor.toml
npm run deploy   # == bash deploy.sh
```

`deploy.sh` does `anchor build`, then
`anchor deploy --program-name fructus --provider.cluster devnet --provider.wallet "$DEVNET_WALLET"`,
then derives and prints the `PerpMarket` / `OrderBook` / vault PDAs and writes
them to `scripts/deploy-output.json`.

### 3. End-to-end life cycle (R-D1c)

Set the environment, then run the network walk:

```bash
cd scripts
export RPC_URL="https://api.devnet.solana.com"
export PROGRAM_ID="<deployed-devnet-id>"
export AUTHORITY_KEYPAIR="$(cat "$HOME/.config/solana/devnet.json")"   # or read from a file
export LONG_USER_KEYPAIR="$(cat ./long.json)"       # never committed
export SHORT_USER_KEYPAIR="$(cat ./short.json)"     # never committed
export INDEX_SOURCE="<devnet jitoSOL stake pool account>"
export USDC_MINT="<devnet USDC mint, 6 decimals>"   # must match market.collateral_mint
export FUNDING_EPOCH_SLOTS=16                       # small epoch → funding settles quickly

npm run e2e:network   # == RUN_E2E=1 tsx e2e.mts
```

The walk (see `e2e.mts`) initializes market/order book/vault (idempotent),
deposits collateral for both traders, opens a **full LONG** and a **full SHORT**
(as takers against opposite-side resting makers), cranks + `settle_funding` each
position, closes each, and `settle_close`s. It then logs/asserts the funding sign
convention.

**Real-run requirements** (documented, not auto-funded): each trader's devnet
USDC ATA must hold ≥ `DEPOSIT_AMOUNT`, and devnet SOL must cover rent + fees. The
script does **not** mint USDC; supply it via a devnet USDC faucet/airdrop first.

---

## Environment variables

| Var | Required (network run) | Meaning |
| --- | --- | --- |
| `RPC_URL` | yes | Devnet RPC endpoint. |
| `PROGRAM_ID` | yes (network); optional (offline) | Deployed devnet program id. Offline defaults to `[programs.localnet]`. |
| `AUTHORITY_KEYPAIR` | yes | Deploy/market authority keypair (JSON byte array). |
| `LONG_USER_KEYPAIR` | yes | Long trader keypair (JSON byte array). |
| `SHORT_USER_KEYPAIR` | yes | Short trader keypair (JSON byte array). |
| `INDEX_SOURCE` | yes | Real devnet jitoSOL SPL Stake Pool account (trustless index). |
| `USDC_MINT` | yes | Devnet USDC mint (6 decimals); set into the market at init. |
| `DEPOSIT_AMOUNT` | no | Per-user deposit, default `1000000000` (1,000 USDC). |
| `POSITION_SIZE` | no | Position notional, default `1000000` (1 USDC). |
| `PRICE` | no | Resting order price (APY_SCALE fixed point), default `1000000`. |
| `FUNDING_K` | no | Funding convergence speed, default `100000`. |
| `MAX_FUNDING` | no | Per-epoch funding cap, default `1000000`. |
| `INITIAL_MARGIN_BPS` | no | Default `1000`. |
| `MAINTENANCE_MARGIN_BPS` | no | Default `500`. |
| `FUNDING_EPOCH_SLOTS` | no | Funding epoch length in slots, default `16`. |

---

## Funding sign convention (R-F3)

- **Offline** (`npm run e2e`): a pure TypeScript mirror of `funding.rs` hard-
  asserts `premium > 0 ⟹ long payment < 0` (long pays), `short payment > 0`
  (short receives), and that the two are exact opposites.
- **Network** (`npm run e2e:network`): the walk logs the observed on-chain
  long/short `UserCollateral.deposited` deltas after `settle_funding` and asserts
  the convention when funding actually flowed. In a flat market (`premium == 0`)
  no funding flows, so the assertion is skipped with a note — this is expected.

## Scope

Touch list: `scripts/**` + `Anchor.toml`. No program code, `publisher/`, `docs/`,
`sdk/`, `cli/`, or `trident-tests/` are modified. No secrets/keypairs are
committed; keypair + RPC come from env only (consistent with `publisher/`).
