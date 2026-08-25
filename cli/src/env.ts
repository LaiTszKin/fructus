//! `.env` + env-var loading and the resolved trader config (R-C2).
//!
//! Mirrors `publisher/` conventions for names (`RPC_URL`, `PROGRAM_ID`,
//! keypair as a JSON byte array) plus the trader-specific `MARKET_ADDRESS`,
//! `INDEX_SOURCE`, `COLLATERAL_MINT`, and `TRADER_KEYPAIR`. No secrets are ever
//! committed; a clearly-labelled placeholder is used for offline/dry-run.

import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import { PROGRAM_ID, marketPda } from "fructus-sdk/src/index.js";
import type { ParsedArgs } from "./args.js";
import { flagBool, flagStrAny } from "./args.js";
import { CliError, die } from "./errors.js";
import { readFileSync, existsSync } from "node:fs";
import { resolve } from "node:path";

/** A clearly-labelled placeholder pubkey (all-zero, 1111...) used for dry-run. */
export const PLACEHOLDER_PUBKEY = new PublicKey(new Uint8Array(32));

/** Names of the environment variables the trader CLI understands. */
export const ENV_KEYS = [
  "RPC_URL",
  "PROGRAM_ID",
  "MARKET_ADDRESS",
  "INDEX_SOURCE",
  "COLLATERAL_MINT",
  "TRADER_KEYPAIR",
] as const;

/**
 * Load a `.env` file into `process.env` **without overwriting** already-set
 * variables. Kept dependency-free; matches the publisher's `cp .env.example
 * .env` convention. A missing file is a no-op.
 */
export function loadDotEnv(cwd = process.cwd()): void {
  const path = resolve(cwd, ".env");
  if (!existsSync(path)) {
    return;
  }
  const text = readFileSync(path, "utf8");
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (trimmed.length === 0 || trimmed.startsWith("#")) {
      continue;
    }
    const eq = trimmed.indexOf("=");
    if (eq === -1) {
      continue;
    }
    const key = trimmed.slice(0, eq).trim();
    let value = trimmed.slice(eq + 1).trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    if (key.length > 0 && process.env[key] === undefined) {
      process.env[key] = value;
    }
  }
}

/** Load a trader keypair from a JSON byte-array string (never committed). */
export function keypairFromJson(json: string): Keypair {
  try {
    const secretKey = Uint8Array.from(JSON.parse(json) as number[]);
    return Keypair.fromSecretKey(secretKey);
  } catch (err) {
    throw new CliError(
      `invalid keypair JSON: ${(err as Error).message}`,
    );
  }
}

/** Load a keypair from a file (a JSON byte-array or a base58 secret). */
export function keypairFromFile(p: string): Keypair {
  const json = readFileSync(p, "utf8").trim();
  return keypairFromJson(json);
}

export interface TraderConfig {
  /** RPC endpoint; `null` in offline/dry-run. */
  rpcUrl: string | null;
  programId: PublicKey;
  /** The perp market address (default = derived market PDA). */
  market: PublicKey;
  /** The index source (stake pool) feeding the trustless index. */
  indexSource: PublicKey;
  /** The collateral mint (USDC). */
  collateralMint: PublicKey;
  /** Signing keypair; `null` in dry-run → placeholder owner. */
  keypair: Keypair | null;
  /** The owner pubkey used to build instructions (keypair or placeholder). */
  owner: PublicKey;
  /** Connect to the RPC for live queries / submission. */
  network: boolean;
  /** Submit the built instruction(s) to the chain. */
  submit: boolean;
}

function envOr(key: string): string | undefined {
  return process.env[key];
}

/**
 * Resolve the trader config from env + flags. Flags override env. A keypair is
 * required only when `submit` is set; otherwise a placeholder owner is used.
 */
export function resolveConfig(
  args: ParsedArgs,
  cwd = process.cwd(),
): TraderConfig {
  loadDotEnv(cwd);
  const f = args.flags;

  const programIdRaw =
    flagStrAny(f, ["program-id", "programId"]) ?? envOr("PROGRAM_ID");
  const programId = programIdRaw ? new PublicKey(programIdRaw) : PROGRAM_ID;

  const marketRaw = flagStrAny(f, ["market", "m"]) ?? envOr("MARKET_ADDRESS");
  const market = marketRaw ? new PublicKey(marketRaw) : marketPda(programId).address;

  const indexRaw =
    flagStrAny(f, ["index-source", "indexSource"]) ?? envOr("INDEX_SOURCE");
  const indexSource = indexRaw ? new PublicKey(indexRaw) : PLACEHOLDER_PUBKEY;

  const mintRaw =
    flagStrAny(f, ["mint", "collateral-mint"]) ?? envOr("COLLATERAL_MINT");
  const collateralMint = mintRaw ? new PublicKey(mintRaw) : PLACEHOLDER_PUBKEY;

  const rpcUrl =
    flagStrAny(f, ["rpc-url", "rpcUrl"]) ?? envOr("RPC_URL") ?? null;

  // Keypair: flag > flag file > env > none (dry-run placeholder).
  let keypair: Keypair | null = null;
  const keypairJson = flagStrAny(f, ["keypair"]);
  if (keypairJson) {
    keypair = keypairFromJson(keypairJson);
  }
  const keypairFile = flagStrAny(f, ["keypair-file", "keypairFile"]);
  if (!keypair && keypairFile) {
    keypair = keypairFromFile(keypairFile);
  }
  if (!keypair && envOr("TRADER_KEYPAIR")) {
    keypair = keypairFromJson(envOr("TRADER_KEYPAIR") as string);
  }

  const network = flagBool(f, "network") || flagBool(f, "submit");
  const submit = flagBool(f, "submit");

  if ((network || submit) && !rpcUrl) {
    die("RPC_URL is required for --network/--submit");
  }
  if (submit && !keypair) {
    die("a keypair is required for --submit (set TRADER_KEYPAIR or --keypair)");
  }

  return {
    rpcUrl,
    programId,
    market,
    indexSource,
    collateralMint,
    keypair,
    owner: keypair ? keypair.publicKey : PLACEHOLDER_PUBKEY,
    network,
    submit,
  };
}

/** Creat a `Connection` (only valid when `rpcUrl` is set). */
export function connect(cfg: TraderConfig): Connection {
  if (!cfg.rpcUrl) {
    die("RPC_URL is not set; cannot connect");
  }
  return new Connection(cfg.rpcUrl, "confirmed");
}
