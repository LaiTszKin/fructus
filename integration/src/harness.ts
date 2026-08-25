//! Integration harness: a `solana-test-validator` running the deployed Fructus
//! program, seeded with a synthetic stake-pool `index_source` (loaded at genesis
//! via `--account`) so the on-chain `read_stake_pool` accepts it, plus the SPL
//! collateral mint + funded trader accounts.
//!
//! Everything below drives the program through the *real* TS SDK builders
//! (`fructus-sdk`), so the "SDK -> protocol" boundary is exercised, not just the
//! Rust bank. See `test/integration-pbt.test.ts`.

import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  Connection,
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import {
  PROGRAM_ID,
  STAKE_POOL_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  ASSOCIATED_TOKEN_PROGRAM_ID,
  USDC_DECIMALS,
  SIDE_BID,
  SIDE_ASK,
  buildInitializeMarket,
  buildInitializeOrderBook,
  buildInitializeCollateralVault,
  buildDepositCollateral,
  buildWithdrawCollateral,
  buildPlaceLimitOrder,
  buildOpenPosition,
  buildSettleFill,
  buildClosePosition,
  buildSettleClose,
  marketPda,
  orderBookPda,
  vaultPda,
  userCollateralPda,
  positionPda,
} from "fructus-sdk/src/index.js";

/** Associated token account address for `mint` owned by `owner`. */
export async function getAssociatedTokenAddress(mint: PublicKey, owner: PublicKey): Promise<PublicKey> {
  return PublicKey.findProgramAddressSync(
    [owner.toBuffer(), TOKEN_PROGRAM_ID.toBuffer(), mint.toBuffer()],
    ASSOCIATED_TOKEN_PROGRAM_ID,
  )[0];
}

export const PROGRAM_ID_DEFAULT = PROGRAM_ID;

// Program .so that `anchor build` produced (localnet profile id == declare_id!).
const ROOT = new URL("..", import.meta.url).pathname;
export const SO_PATH = join(ROOT, "..", "target", "deploy", "fructus.so");

export const DEFAULT_RPC_URL = "http://127.0.0.1:8899";

/** Market parameters (same ballpark as the Rust CPI/PBT tests + AGENTS.md). */
export interface MarketParams {
  fundingK: bigint;
  maxFunding: bigint;
  fundingEpochSlots: bigint;
  initialMarginBps: number;
  maintenanceMarginBps: number;
}

export const DEFAULT_MARKET: MarketParams = {
  fundingK: 100_000n,
  maxFunding: 10_000n,
  fundingEpochSlots: 1_000n,
  initialMarginBps: 1_000, // 10x
  maintenanceMarginBps: 500,
};

export interface Validator {
  rpcUrl: string;
  connection: Connection;
  programId: PublicKey;
  indexSource: PublicKey;
  /** The index source rate baked into the synthetic stake-pool account. */
  indexTotalLamports: bigint;
  indexPoolTokenSupply: bigint;
  /** A funded authority keypair (signs initialize_* + cranks). */
  authority: Keypair;
  /** SPL collateral mint (6dp). */
  mint: PublicKey;
  configPath: string;
  authorityKeypairPath: string;
  stop(): void;
}

// ---------------------------------------------------------------------------
// Account-dump + config writers
// ---------------------------------------------------------------------------

/** Build the genesis account-dump JSON for the synthetic stake-pool account. */
function indexSourceDump(
  pubkey: PublicKey,
  totalLamports: bigint,
  poolTokenSupply: bigint,
): { path: string; json: string } {
  // SPL StakePool account layout (with the AccountType discriminator): the
  // program reads byte0 == 1 and u64 LE at 258/266 (see exchange.rs).
  const space = 1024;
  const data = new Uint8Array(space);
  data[0] = 1; // AccountType::StakePool
  const dv = new DataView(data.buffer, data.byteOffset, data.byteLength);
  dv.setBigUint64(258, totalLamports, true);
  dv.setBigUint64(266, poolTokenSupply, true);
  const json = JSON.stringify({
    pubkey: pubkey.toBase58(),
    account: {
      lamports: 5_000_000_000, // 5 SOL, rent-exempt for a 1 KiB account
      data: [Buffer.from(data).toString("base64"), "base64"],
      owner: STAKE_POOL_PROGRAM_ID.toBase58(),
      executable: false,
      rentEpoch: 0,
    },
  });
  return { path: join(tmpdir(), `fructus-index-${pubkey.toBase58().slice(0, 8)}.json`), json };
}

/** Write a solana CLI config file bound to `rpcUrl` + `keypairPath`. */
function writeSolanaConfig(dir: string, rpcUrl: string, keypairPath: string): string {
  const cfg = join(dir, "solana.cfg");
  writeFileSync(
    cfg,
    [
      `json_rpc_url: ${rpcUrl}`,
      'websocket_url: ""',
      `keypair_path: ${keypairPath}`,
      "commitment: confirmed",
      "",
    ].join("\n"),
  );
  return cfg;
}

function writeKeypair(path: string, kp: Keypair): void {
  writeFileSync(path, JSON.stringify(Array.from(kp.secretKey)));
}

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

function run(cmd: string, args: string[]): string {
  const r = spawnSync(cmd, args, { encoding: "utf-8" });
  if (r.status !== 0) {
    throw new Error(`${cmd} ${args.join(" ")} failed (${r.status}): ${r.stderr || r.stdout}`);
  }
  return r.stdout;
}

function parseJson(out: string): Record<string, unknown> {
  const m = out.match(/\{.*\}/s);
  if (!m) throw new Error(`no JSON in output: ${out.slice(0, 200)}`);
  return JSON.parse(m[0]) as Record<string, unknown>;
}

async function waitForRpc(connection: Connection, timeoutMs = 45_000): Promise<void> {
  const start = Date.now();
  // eslint-disable-next-line no-constant-condition
  while (true) {
    try {
      await connection.getLatestBlockhash("confirmed");
      return;
    } catch {
      if (Date.now() - start > timeoutMs) {
        throw new Error(`validator RPC not ready after ${timeoutMs}ms`);
      }
      await new Promise((r) => setTimeout(r, 400));
    }
  }
}

async function airdrop(connection: Connection, pk: PublicKey, sol: number): Promise<void> {
  const lamports = Math.round(sol * LAMPORTS_PER_SOL);
  const bal = await connection.getBalance(pk);
  if (bal >= lamports) return;
  // eslint-disable-next-line no-constant-condition
  for (let i = 0; i < 12 && (await connection.getBalance(pk)) < lamports; i++) {
    try {
      // eslint-disable-next-line no-await-in-loop
      await connection.requestAirdrop(pk, lamports);
    } catch {
      /* rate-limited; retry */
    }
    // eslint-disable-next-line no-await-in-loop
    await new Promise((r) => setTimeout(r, 300));
  }
  const after = await connection.getBalance(pk);
  if (after < lamports) {
    throw new Error(`airdrop to ${pk.toBase58()} did not reach ${sol} SOL (got ${after / 1e9})`);
  }
}

// ---------------------------------------------------------------------------
// Validator lifecycle
// ---------------------------------------------------------------------------

export interface StartOptions {
  rpcUrl?: string;
  authority?: Keypair;
  indexTotalLamports?: bigint;
  indexPoolTokenSupply?: bigint;
  programId?: PublicKey;
}

export async function startValidator(opts: StartOptions = {}): Promise<Validator> {
  const rpcUrl = opts.rpcUrl ?? DEFAULT_RPC_URL;
  const programId = opts.programId ?? PROGRAM_ID_DEFAULT;
  const authority = opts.authority ?? Keypair.generate();
  const indexTotalLamports = opts.indexTotalLamports ?? 12_000_000_000n; // ~12 SOL
  const indexPoolTokenSupply = opts.indexPoolTokenSupply ?? 10_000_000_000n;

  if (!existsSync(SO_PATH)) {
    throw new Error(`program .so not found: ${SO_PATH} — run \`anchor build\` first`);
  }

  const dir = mkdtempSync(join(tmpdir(), "fructus-val-"));
  const ledger = join(dir, "ledger");
  const authorityPath = join(dir, "authority.json");
  writeKeypair(authorityPath, authority);

  // Synthetic stake-pool index source, loaded at genesis.
  const indexSource = Keypair.generate().publicKey;
  const dump = indexSourceDump(indexSource, indexTotalLamports, indexPoolTokenSupply);
  writeFileSync(dump.path, dump.json);

  const configPath = writeSolanaConfig(dir, rpcUrl, authorityPath);

  const args = [
    "--ledger",
    ledger,
    "--reset",
    "--rpc-port",
    rpcUrl.match(/:(\d+)/)?.[1] ?? "8899",
    "--bpf-program",
    programId.toBase58(),
    SO_PATH,
    "--account",
    indexSource.toBase58(),
    dump.path,
  ];

  const proc = spawn("solana-test-validator", args, { stdio: "ignore", detached: true });
  await waitForRpc(new Connection(rpcUrl, "confirmed"));
  await airdrop(new Connection(rpcUrl, "confirmed"), authority.publicKey, 120);

  return {
    rpcUrl,
    connection: new Connection(rpcUrl, "confirmed"),
    programId,
    indexSource,
    indexTotalLamports,
    indexPoolTokenSupply,
    authority,
    mint: PublicKey.unique(), // placeholder; set by ensureCollateralMint()
    configPath,
    authorityKeypairPath: authorityPath,
    stop() {
      try {
        process.kill(-proc.pid!, "SIGKILL"); // kill the process group
      } catch {
        proc.kill("SIGKILL");
      }
      try {
        rmSync(dir, { recursive: true, force: true });
      } catch {
        /* best-effort */
      }
    },
  };
}

// ---------------------------------------------------------------------------
// Market + collateral bootstrap (via the SDK builders)
// ---------------------------------------------------------------------------

/** Create the SPL collateral mint + mint `amount` to each trader's ATA. */
export async function ensureCollateralMint(v: Validator): Promise<PublicKey> {
  const out = run("spl-token", [
    "create-token",
    "--decimals",
    String(USDC_DECIMALS),
    "--config",
    v.configPath,
    "--output",
    "json",
  ]);
  const j = parseJson(out);
  const cmdOut = (j.commandOutput ?? j) as Record<string, unknown>;
  const addr = (cmdOut.address as string) ?? (j.address as string) ?? (j.mintAddress as string);
  if (!addr) throw new Error(`could not parse collateral mint from: ${out}`);
  const mint = new PublicKey(addr);
  v.mint = mint;
  return mint;
}

/** Ensure `owner` has an ATA for the mint and mint `micro` units to it. */
export async function fundTrader(
  v: Validator,
  owner: PublicKey,
  micro: bigint,
  label = "trader",
): Promise<PublicKey> {
  // The trader is the fee payer AND the rent payer for its `UserCollateral`
  // account on deposit, so it must hold SOL.
  await airdrop(v.connection, owner, 10);
  let ata: PublicKey | undefined;
  try {
    const out = run("spl-token", [
      "create-account",
      v.mint.toBase58(),
      "--owner",
      owner.toBase58(),
      "--config",
      v.configPath,
      "--fee-payer",
      v.authorityKeypairPath,
      "--output",
      "json",
    ]);
    const j = parseJson(out);
    const addr = ((j.commandOutput ?? j) as Record<string, unknown>).address as string | undefined;
    if (addr) ata = new PublicKey(addr);
  } catch {
    /* ATA already exists; fall through to chain lookup */
  }
  if (!ata) {
    const accts = await v.connection.getTokenAccountsByOwner(owner, { mint: v.mint });
    ata = accts.value[0]?.pubkey;
  }
  if (!ata) throw new Error(`[fundTrader] no ${label} ATA found for ${v.mint.toBase58()}`);
  run("spl-token", [
    "mint",
    v.mint.toBase58(),
    (micro + 10n ** BigInt(USDC_DECIMALS)).toString(),
    ata.toBase58(),
    "--config",
    v.configPath,
    "--output",
    "json",
  ]);
  return ata;
}

/** Submit a single instruction (sign + send) via the SDK submit helper. */
export async function submit(v: Validator, ix: TransactionInstruction, signer: Keypair): Promise<string> {
  const tx = new Transaction().add(ix);
  tx.feePayer = signer.publicKey;
  const bh = await v.connection.getLatestBlockhash("confirmed");
  tx.recentBlockhash = bh.blockhash;
  tx.sign(signer);
  const raw = tx.serialize();
  const sig = await v.connection.sendRawTransaction(raw, {
    skipPreflight: false,
    preflightCommitment: "confirmed",
  });
  await v.connection.confirmTransaction(
    { signature: sig, blockhash: bh.blockhash, lastValidBlockHeight: bh.lastValidBlockHeight },
    "confirmed",
  );
  return sig;
}

export interface MarketEnv {
  market: PublicKey;
  orderBook: PublicKey;
  vault: PublicKey;
}

/** Initialize the perp market + order book + collateral vault. */
export async function initializeMarket(v: Validator, params: MarketParams = DEFAULT_MARKET): Promise<MarketEnv> {
  const market = marketPda(v.programId).address;
  const orderBook = orderBookPda(market, v.programId).address;
  const vault = vaultPda(v.programId).address;

  await submit(
    v,
    buildInitializeMarket({
      indexSource: v.indexSource,
      authority: v.authority.publicKey,
      payer: v.authority.publicKey,
      collateralMint: v.mint,
      fundingK: params.fundingK,
      maxFunding: params.maxFunding,
      fundingEpochSlots: params.fundingEpochSlots,
      initialMarginBps: params.initialMarginBps,
      maintenanceMarginBps: params.maintenanceMarginBps,
      programId: v.programId,
    }),
    v.authority,
  );

  await submit(
    v,
    buildInitializeOrderBook({
      market,
      authority: v.authority.publicKey,
      payer: v.authority.publicKey,
      programId: v.programId,
    }),
    v.authority,
  );

  await submit(
    v,
    buildInitializeCollateralVault({
      market,
      authority: v.authority.publicKey,
      payer: v.authority.publicKey,
      collateralMint: v.mint,
      programId: v.programId,
    }),
    v.authority,
  );

  return { market, orderBook, vault };
}

export { SystemProgram, SIDE_BID, SIDE_ASK, userCollateralPda, positionPda };
