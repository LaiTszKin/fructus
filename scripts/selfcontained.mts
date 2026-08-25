/**
 * Fructus devnet "self-contained" e2e setup (issue #9, R-D1).
 *
 * Bootstraps everything the devnet lifecycle walk needs WITHOUT depending on an
 * external USDC faucet or an external stake pool:
 *
 *   1. wallets      — ensure authority (deploy) + long + short keypairs exist
 *   2. sol airdrop  — fund all three wallets with devnet SOL (free, for rent+fees)
 *   3. collateral   — create a self-owned 6-decimal SPL mint and fund both
 *                     traders' ATAs (no USDC faucet needed)
 *   4. stake pool   — bootstrap a self-owned devnet SPL Stake Pool (create-pool +
 *                     deposit-sol into the reserve) => real INDEX_SOURCE
 *   5. validate     — assert the pool satisfies Fructus read_exchange_rate
 *                     (owner + AccountType::StakePool + non-zero 258/266), and
 *                     that the trader ATAs hold >= DEPOSIT_AMOUNT
 *   6. env          — print the env block to export for `npm run deploy` +
 *                     `npm run e2e:network`
 *
 * SAFE: every step is idempotent (skips if the artifact already exists). The
 * script only ever touches devnet, never commits keypairs or the temp solana
 * config (gitignored under scripts/), and never touches the user's global
 * solana config — all CLI subprocesses pass our own `--config`.
 *
 * USAGE (all optional; sensible defaults):
 *   AUTHORITY_KEYPAIR=scripts/authority.keypair.json
 *   LONG_KEYPAIR=scripts/long.keypair.json
 *   SHORT_KEYPAIR=scripts/short.keypair.json
 *   RPC_URL=https://api.devnet.solana.com
 *   DEPOSIT_AMOUNT=1000000000        # 1000 collateral microunits (6dp)
 *   POOL_DEPOSIT_SOL=1               # devnet SOL into the stake-pool reserve
 *   POOL_MAX_VALIDATORS=4
 *
 *   npm run setup            # full bootstrap
 *   npm run setup -- --preflight   # keypair/tool check only, no transactions
 */

import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

// ---------------------------------------------------------------------------
// Config + helpers
// ---------------------------------------------------------------------------

const here = dirname(fileURLToPath(import.meta.url));

function envOr(key: string, fallback: string): string {
  return process.env[key] ? process.env[key] : fallback;
}
function envNum(key: string, fallback: number): number {
  const v = process.env[key];
  return v ? Number(v) : fallback;
}

const RPC_URL = envOr("RPC_URL", "https://api.devnet.solana.com");

const AUTHORITY_PATH = resolve(here, envOr("AUTHORITY_KEYPAIR", "authority.keypair.json"));
const LONG_PATH = resolve(here, envOr("LONG_KEYPAIR", "long.keypair.json"));
const SHORT_PATH = resolve(here, envOr("SHORT_KEYPAIR", "short.keypair.json"));

const DEPOSIT_AMOUNT = BigInt(envOr("DEPOSIT_AMOUNT", "1000000000")); // 1000 micro (6dp)
const COLLATERAL_DECIMALS = 6;
const MINT_HEADROOM_TOKENS = 5; // mint a few tokens above the deposit

// Stake-pool (R-D1) bootstrap parameters.
const POOL_MAX_VALIDATORS = envNum("POOL_MAX_VALIDATORS", 4);
const EPOCH_FEE_BPS_DEFAULT = { numerator: 0, denominator: 1 }; // zero fee => needs --unsafe-fees
const POOL_DEPOSIT_SOL = envNum("POOL_DEPOSIT_SOL", 1);

// Fructus exchange.rs read_exchange_rate contract (do not change).
const STAKE_POOL_PROGRAM_ID = "SPoo1Ku8WFXoNDMHPsrGSTSG1Y47rzgn41SLUNakuHy";
const ACCOUNT_TYPE_STAKE_POOL = 1;
const TOTAL_LAMPORTS_OFFSET = 258;
const POOL_TOKEN_SUPPLY_OFFSET = 266;

const CONFIG_PATH = resolve(here, "devnet-config.yml");

const conn = new Connection(RPC_URL, "confirmed");

/** A small synchronous CLI runner returning combined stdout+stderr. */
function run(cmd: string, args: string[]): string {
  const res = spawnSync(cmd, args, { encoding: "utf-8" });
  const out = `${res.stdout ?? ""}\n${res.stderr ?? ""}`.trim();
  if (res.status !== 0) {
    const code = (res.error as { code?: string } | null)?.code ?? res.status;
    throw new Error(`command failed (${code}): ${cmd} ${args.join(" ")}\n${out}`);
  }
  return out;
}

/** Whether a binary is on PATH (or exists at an absolute path). */
function hasCmd(cmd: string): boolean {
  const res = spawnSync(cmd, ["--version"], { encoding: "utf-8" });
  return !res.error && res.status === 0;
}

function ensureKeypair(path: string, label: string): Keypair {
  if (existsSync(path)) {
    const kp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(path, "utf-8")) as number[]));
    console.log(`[wallet] ${label} already exists: ${kp.publicKey.toBase58()}`);
    return kp;
  }
  const kp = Keypair.generate();
  writeFileSync(path, JSON.stringify([...kp.secretKey]), { mode: 0o600 });
  console.log(`[wallet] generated ${label}: ${kp.publicKey.toBase58()}`);
  return kp;
}

/** Write a per-run solana config pointing at devnet + the authority keypair, so
 *  every CLI default (payer / mint-authority / manager / staker / token-owner)
 *  resolves to the authority and no global config is touched. */
function ensureConfig(): void {
  writeFileSync(
    CONFIG_PATH,
    [
      `json_rpc_url: ${RPC_URL}`,
      "websocket_url: \"\"",
      `keypair_path: ${AUTHORITY_PATH}`,
      "commitment: confirmed",
      "",
    ].join("\n"),
    { mode: 0o600 },
  );
}

function parseJson(out: string): Record<string, unknown> {
  const m = out.match(/\{.*\}/s);
  if (!m) throw new Error(`no JSON in output: ${out.slice(0, 200)}`);
  return JSON.parse(m[0]) as Record<string, unknown>;
}

async function solBalance(pk: PublicKey): Promise<number> {
  return (await conn.getBalance(pk)) / 1e9;
}

async function airdropSol(pk: PublicKey, target: number, label: string): Promise<void> {
  const bal = await solBalance(pk);
  console.log(`[airdrop] ${label} balance: ${bal.toFixed(3)} SOL (target ${target})`);
  if (bal >= target) {
    console.log(`    skip (already funded)`);
    return;
  }
  // devnet airdrops are rate-limited per IP and per address; retry a few small
  // requests and, if we still cannot reach the target, fail with clear guidance
  // rather than silently proceeding (later steps would fail on insufficient SOL).
  const tries = 8;
  for (let i = 0; i < tries && (await solBalance(pk)) < target; i++) {
    spawnSync("solana", ["airdrop", "1", pk.toBase58(), "--url", RPC_URL], { encoding: "utf-8" });
    await new Promise((r) => setTimeout(r, 6000));
  }
  const after = await solBalance(pk);
  if (after < target) {
    throw new Error(
      `[airdrop] ${label} still ${after.toFixed(3)} SOL (need ${target}). Devnet airdrop is ` +
        `rate-limited; re-run later, or airdrop from another connection: ` +
        `solana airdrop ${target} ${pk.toBase58()} --url ${RPC_URL}`,
    );
  }
  console.log(`[airdrop] ${label} now ${after.toFixed(3)} SOL`);
}

// --- collateral mint (self-owned 6dp SPL token) ---------------------------------

async function ensureCollateralMint(): Promise<PublicKey> {
  const existing = envOr("COLLATERAL_MINT", "");
  if (existing) {
    console.log(`[collateral] using COLLATERAL_MINT=${existing}`);
    return new PublicKey(existing);
  }
  // create-token: fee payer + mint authority default to the config keypair (authority).
  const out = run("spl-token", ["create-token", "--decimals", String(COLLATERAL_DECIMALS), "--config", CONFIG_PATH, "--output", "json"]);
  const j = parseJson(out);
  const addr = (j.address as string) ?? (j.mintAddress as string);
  if (!addr) throw new Error(`could not parse collateral mint from: ${out}`);
  console.log(`[collateral] created mint ${addr}`);
  return new PublicKey(addr);
}

async function ensureTraderFunded(mint: PublicKey, trader: Keypair, label: string): Promise<void> {
  // create-account with --owner <trader> creates/uses the standard ATA (matches e2e).
  // Idempotent: an ATA that already exists is a no-op, so ignore the error.
  try {
    run("spl-token", [
      "create-account", mint.toBase58(),
      "--owner", trader.publicKey.toBase58(),
      "--config", CONFIG_PATH,
      "--output", "json",
    ]);
  } catch {
    /* already exists */
  }
  // Find the trader's ATA for this mint (create-account above guarantees it exists).
  const accts = await conn.getTokenAccountsByOwner(trader.publicKey, { mint });
  const ata = accts.value[0]?.pubkey;
  if (!ata) throw new Error(`[collateral] ${label} ATA not found after create-account`);
  const bal = BigInt((await conn.getTokenAccountBalance(ata)).value.amount);
  console.log(`[collateral] ${label} ATA ${ata.toBase58()} balance=${bal} micro (need >= ${DEPOSIT_AMOUNT})`);
  if (bal >= DEPOSIT_AMOUNT) return;
  const factor = 10n ** BigInt(COLLATERAL_DECIMALS);
  const tokens = (DEPOSIT_AMOUNT / factor + BigInt(MINT_HEADROOM_TOKENS)).toString();
  run("spl-token", ["mint", mint.toBase58(), tokens, ata.toBase58(), "--config", CONFIG_PATH, "--output", "json"]);
  console.log(`[collateral] minted ${tokens} tokens to ${label} ATA`);
}

// --- stake pool bootstrap (self-owned devnet SPL Stake Pool) ----------------------

async function ensureStakePool(authority: Keypair): Promise<PublicKey> {
  const existing = envOr("INDEX_SOURCE", "");
  if (existing) {
    console.log(`[pool] using INDEX_SOURCE=${existing}`);
    return new PublicKey(existing);
  }
  if (!hasCmd("spl-stake-pool")) {
    throw new Error("spl-stake-pool CLI not on PATH. Install: cargo install spl-stake-pool-cli");
  }

  // 1. Deterministic keypairs so we always know the resulting pool address.
  const poolKp = resolve(here, "pool.keypair.json");
  const mintKp = resolve(here, "poolmint.keypair.json");
  const reserveKp = resolve(here, "reserve.keypair.json");
  const vlistKp = resolve(here, "vlist.keypair.json");
  for (const p of [poolKp, mintKp, reserveKp, vlistKp]) {
    if (!existsSync(p)) spawnSync("solana-keygen", ["new", "--no-bip39-passphrase", "--force", "-o", p], { encoding: "utf-8" });
  }
  const poolAddr = spawnSync("solana-keygen", ["pubkey", poolKp], { encoding: "utf-8" }).stdout.trim();

  console.log(`[pool] creating stake pool ${poolAddr} ...`);
  run("spl-stake-pool", [
    "create-pool",
    "--config", CONFIG_PATH,
    "--epoch-fee-numerator", String(EPOCH_FEE_BPS_DEFAULT.numerator),
    "--epoch-fee-denominator", String(EPOCH_FEE_BPS_DEFAULT.denominator),
    "--max-validators", String(POOL_MAX_VALIDATORS),
    "--pool-keypair", poolKp,
    "--mint-keypair", mintKp,
    "--reserve-keypair", reserveKp,
    "--validator-list-keypair", vlistKp,
    "--output", "json",
    "--unsafe-fees",
  ]);

  console.log(`[pool] depositing ${POOL_DEPOSIT_SOL} devnet SOL into the reserve ...`);
  run("spl-stake-pool", [
    "deposit-sol", poolAddr, String(POOL_DEPOSIT_SOL),
    "--config", CONFIG_PATH,
    "--output", "json",
  ]);

  console.log(`[pool] bootstrap done: ${poolAddr}`);
  return new PublicKey(poolAddr);
}

// --- validate against Fructus read_exchange_rate + e2e prereqs -------------------

async function validatePool(pool: PublicKey): Promise<void> {
  const info = await conn.getAccountInfo(pool);
  if (!info) throw new Error(`[validate] pool ${pool.toBase58()} not found`);
  if (info.owner.toBase58() !== STAKE_POOL_PROGRAM_ID)
    throw new Error(`[validate] owner=${info.owner.toBase58()} != ${STAKE_POOL_PROGRAM_ID}`);
  if (info.data[0] !== ACCOUNT_TYPE_STAKE_POOL)
    throw new Error(`[validate] account_type byte=${info.data[0]} != ${ACCOUNT_TYPE_STAKE_POOL}`);
  if (info.data.length < POOL_TOKEN_SUPPLY_OFFSET + 8)
    throw new Error(`[validate] account too short (${info.data.length} bytes)`);
  const total = info.data.readBigUInt64LE(TOTAL_LAMPORTS_OFFSET);
  const supply = info.data.readBigUInt64LE(POOL_TOKEN_SUPPLY_OFFSET);
  if (supply === 0n) throw new Error("[validate] pool_token_supply == 0 (deposit failed?)");
  if (total === 0n) throw new Error("[validate] total_lamports == 0 (deposit failed?)");
  console.log(`[validate] pool OK: total_lamports=${total} pool_token_supply=${supply}`);
}

async function validateTraders(mint: PublicKey, long: Keypair, short: Keypair): Promise<void> {
  for (const [label, kp] of [["long", long], ["short", short]] as const) {
    const accts = await conn.getTokenAccountsByOwner(kp.publicKey, { mint });
    const ata = accts.value[0]?.pubkey;
    if (!ata) throw new Error(`[validate] ${label} has no ATA for ${mint.toBase58()}`);
    const bal = BigInt((await conn.getTokenAccountBalance(ata)).value.amount);
    if (bal < DEPOSIT_AMOUNT) throw new Error(`[validate] ${label} ATA ${bal} < deposit ${DEPOSIT_AMOUNT}`);
    console.log(`[validate] ${label} ATA ${ata.toBase58()} balance=${bal} OK`);
  }
}

// --- main ----------------------------------------------------------------------

async function main(): Promise<void> {
  const preflight = process.argv.includes("--preflight");
  console.log("=== Fructus self-contained devnet e2e setup ===");
  console.log(`RPC: ${RPC_URL}`);

  // Tooling preflight
  for (const [tool, install] of [
    ["solana", "solana"],
    ["spl-token", "spl-token-cli"],
    ["spl-stake-pool", "spl-stake-pool-cli (cargo install spl-stake-pool-cli)"],
  ] as const) {
    if (!hasCmd(tool)) throw new Error(`missing tool '${tool}' — install: ${install}`);
  }

  // 1. wallets
  const authority = ensureKeypair(AUTHORITY_PATH, "authority");
  const long = ensureKeypair(LONG_PATH, "long");
  const short = ensureKeypair(SHORT_PATH, "short");
  ensureConfig();
  console.log(`    long:  ${long.publicKey.toBase58()}`);
  console.log(`    short: ${short.publicKey.toBase58()}`);

  if (preflight) {
    console.log("\n[preflight] keypairs + tools ready. No transactions sent.");
    console.log(`  AUTHORITY_KEYPAIR=${AUTHORITY_PATH}`);
    console.log(`  LONG_KEYPAIR=${LONG_PATH}`);
    console.log(`  SHORT_KEYPAIR=${SHORT_PATH}`);
    console.log(`  (devnet SOL airdrop + pool bootstrap happen on the real run)`);
    return;
  }

  // 2. airdrop devnet SOL (free; idempotent)
  await airdropSol(authority.publicKey, 2, "authority");
  await airdropSol(long.publicKey, 0.1, "long");
  await airdropSol(short.publicKey, 0.1, "short");

  // 3. collateral mint + fund traders
  const mint = await ensureCollateralMint();
  await ensureTraderFunded(mint, long, "long");
  await ensureTraderFunded(mint, short, "short");

  // 4. stake pool (INDEX_SOURCE)
  const pool = await ensureStakePool(authority);

  // 5. validate
  await validatePool(pool);
  await validateTraders(mint, long, short);

  // 6. env block
  console.log("\n=== READY. Export these for deploy + e2e ===");
  console.log(`export RPC_URL="${RPC_URL}"`);
  console.log(`export PROGRAM_ID="<deployed program id — see Anchor.toml / deploy.sh>"`);
  console.log(`export INDEX_SOURCE="${pool.toBase58()}"`);
  console.log(`export USDC_MINT="${mint.toBase58()}"`);
  console.log(`export AUTHORITY_KEYPAIR="\$(cat "${AUTHORITY_PATH}")"`);
  console.log(`export LONG_USER_KEYPAIR="\$(cat "${LONG_PATH}")"`);
  console.log(`export SHORT_USER_KEYPAIR="\$(cat "${SHORT_PATH}")"`);
  console.log(`export DEPOSIT_AMOUNT=${DEPOSIT_AMOUNT}`);
  console.log(`export FUNDING_EPOCH_SLOTS=16`);
  console.log("\nThen: cd scripts && npm run deploy && npm run e2e:network");
}

await main();
