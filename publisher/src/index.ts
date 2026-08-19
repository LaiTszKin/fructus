import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import { fetchLatestApy, toScaledApy } from "./jito.js";
import { decodeOracle, isStale } from "./state.js";
import { submitUpdate } from "./update.js";

/**
 * The Fructus yield-oracle keeper.
 *
 * Normal path is user-driven pull; this keeper is the *fallback* that keeps the
 * on-chain APY fresh for third-party readers and low-liquidity windows. It
 * writes to the chain when the value moved or when the oracle's staleness
 * window has elapsed, so `last_update_slot` keeps advancing through flat-APY
 * periods instead of tripping consumers' `is_stale` circuit breaker.
 */

function env(key: string): string {
  const v = process.env[key];
  if (!v) throw new Error(`missing env: ${key}`);
  return v;
}

function loadKeypair(json: string): Keypair {
  const secretKey = Uint8Array.from(JSON.parse(json) as number[]);
  return Keypair.fromSecretKey(secretKey);
}

async function runOnce(): Promise<void> {
  const rpcUrl = env("RPC_URL");
  const programId = new PublicKey(env("PROGRAM_ID"));
  const oracle = new PublicKey(env("ORACLE_ADDRESS"));
  const jitoApi = env("JITO_API");
  const publisher = loadKeypair(env("PUBLISHER_KEYPAIR"));
  const connection = new Connection(rpcUrl, "confirmed");

  const current = decodeOracle((await connection.getAccountInfo(oracle))?.data ?? null);
  if (!current) {
    throw new Error("oracle account not found or too short");
  }

  const apyDecimal = await fetchLatestApy(jitoApi);
  const apy = toScaledApy(apyDecimal);

  // Change + staleness detection: publish if the value moved, or if the
  // staleness window has elapsed so a flat APY still refreshes
  // `last_update_slot` (keeping consumers' circuit breaker from tripping).
  const currentSlot = BigInt(await connection.getSlot());
  const stale = isStale(current.last_update_slot, current.stale_after_slots, currentSlot);

  if (apy === current.apy && !stale) {
    console.log(`[keeper] APY unchanged and oracle fresh (${apyDecimal}), skipping`);
    return;
  }

  const version = current.version + 1n;
  const reason = stale ? "stale refresh" : "apy change";
  console.log(`[keeper] publishing apy=${apy} version=${version} (was ${current.apy}) [${reason}]`);
  const sig = await submitUpdate(connection, { oracle, programId, publisher, apy, version });
  console.log(`[keeper] submitted: ${sig}`);
}

async function main(): Promise<void> {
  const pollMs = Number(process.env.POLL_INTERVAL_MS ?? 3_600_000);

  // Run once, then poll on a low-frequency interval. Epoch-boundary precision
  // can be layered on by resolving the next epoch start slot via the RPC.
  // eslint-disable-next-line no-constant-condition
  while (true) {
    try {
      await runOnce();
    } catch (err) {
      console.error("[keeper] run failed:", err);
    }
    await new Promise((r) => setTimeout(r, pollMs));
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
