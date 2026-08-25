# Fructus trader CLI

Off-chain command-line trader for the Fructus Solana yield-perp protocol (GitHub
issue #11). It builds the corresponding program instruction via the `fructus-sdk`,
prints a **dry-run** report (built instruction + derived PDAs + expected
funding/PnL) by default, and only signs/submits on-chain when `--submit` is given.

## Commands

| Command    | Builds (via SDK)                 | Notes |
|------------|----------------------------------|-------|
| `open`     | `open_position`                  | `open <side> <size> <price>` |
| `close`    | `close_position` (+ `settle_close`) | `close <side> <size> [--settle]` |
| `deposit`  | `deposit_collateral`             | `deposit <amount>` |
| `withdraw` | `withdraw_collateral`            | `withdraw <amount>` |
| `position` | queries `position`               | `position <side>` (network mode) |
| `funding`  | `settle_funding`                 | `funding <side>` (network mode) |
| `mark`     | queries the order book           | `mark` (network mode) |
| `index`    | queries market + stake pool      | `index` (network mode) |

Each command supports offline options for dry-run (e.g. `funding` accepts
`--notional/--premium/--funding-k/--max-funding/--epochs`; `mark` accepts
`--bid/--ask`). `--network/-n` connects for live queries; `--submit/-s` signs +
submits.

## Setup

```bash
npm install
cp .env.example .env   # fill in RPC_URL, TRADER_KEYPAIR, PROGRAM_ID, …
```

> Never commit `.env` or a real keypair. Dry-run uses a clearly-labelled
> placeholder owner (`1111…`) and placeholder PDAs; real submission requires a
> keypair (`TRADER_KEYPAIR` env, `--keypair`, or `--keypair-file`).

## Run

```bash
npm test               # tsx --test test/*.test.ts (help + arg parsing smoke)
npm run build          # tsc -p tsconfig.json (strict typecheck)
npm run cli -- --help
npm run cli -- open long 100 100
npm run cli -- open long 100 100 --submit   # needs RPC_URL + keypair
```

## SDK wiring

`cli` depends on `fructus-sdk` via the local `file:../sdk` dependency and imports
its TypeScript source at `fructus-sdk/src/index.js`, so both the `tsx` runtime
smoke test and the `tsc` typecheck resolve against the SDK's source tree. The SDK
package itself is not modified (it ships no `main`/`exports`), so the CLI imports
the resolvable source subpath instead.
