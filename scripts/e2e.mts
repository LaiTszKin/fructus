/**
 * Fructus devnet end-to-end lifecycle walk (issue #9, R-D1c).
 *
 * A best-effort, readable walk: connect to devnet RPC (from env), derive the
 * PDAs, initialize the market + order book + collateral vault, deposit
 * collateral, open a full LONG and a full SHORT, advance funding
 * (crank / settle_funding), close each, settle_close — and log/assert the
 * funding sign convention (premium > 0 ⟹ long pays, short receives; R-F3).
 *
 * SAFE OFFLINE (default): with no run flag the script NEVER touches the
 * network. It derives the PDAs, runs a pure TypeScript mirror of the funding
 * engine, and asserts the sign convention — so it builds + typechecks offline:
 *
 *   npx tsc -p tsconfig.json --noEmit
 *
 * NETWORK RUN (behind a run flag): set RUN_E2E=1 (or pass `--network`). This
 * path is best-effort against a real devnet deployment and requires the
 * documented funding/airdrop setup (see scripts/README.md). On-chain mark /
 * index may produce premium == 0 in a flat market, in which case no funding
 * flows and the sign assertion is skipped with a note.
 *
 *   RUN_E2E=1 npm run e2e
 *
 * NOTE ON THE EXTENSION: this is a TypeScript ESM module (`.mts`). `tsx` does
 * not strip type annotations from `.mjs` (verified), so a runnable, strictly-
 * typechecked walk must use a TypeScript module extension. The run commands in
 * scripts/package.json / scripts/README.md use the `.mts` path accordingly.
 */

import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
  sendAndConfirmTransaction,
} from "@solana/web3.js";
import { createHash } from "node:crypto";

// ---------------------------------------------------------------------------
// Protocol constants (mirror programs/fructus/src/constants.rs)
// ---------------------------------------------------------------------------

const APY_SCALE = 1_000_000n;
const SLOTS_PER_YEAR = 78_840_000n;

const PERP_MARKET_SEED = Buffer.from("perp_market");
const ORDER_BOOK_SEED = Buffer.from("order_book");
const VAULT_SEED = Buffer.from("vault");
const USER_COLLATERAL_SEED = Buffer.from("user_collateral");
const POSITION_SEED = Buffer.from("position");

const SIDE_LONG = 0; // Long / Bid
const SIDE_SHORT = 1; // Short / Ask

const TOKEN_PROGRAM_ID = new PublicKey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const ASSOCIATED_TOKEN_PROGRAM_ID = new PublicKey(
  "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
);

// Localnet program id (from Anchor.toml [programs.localnet]). Real devnet id is
// set via PROGRAM_ID env after a real deploy (see scripts/README.md).
const DEFAULT_PROGRAM_ID = "8ZLiJ12eBiam4UP2HRp3M75CQAcc8GuUBz44zeHt6mjH";

// ---------------------------------------------------------------------------
// Env helpers (no secrets committed; keypair + RPC from env only)
// ---------------------------------------------------------------------------

/** Read a required env var, throwing a clear error when unset. */
function env(key: string): string {
  const v = process.env[key];
  if (!v) throw new Error(`missing env: ${key}`);
  return v;
}

/** Read an optional env var, returning `fallback` when unset/empty. */
function envOr(key: string, fallback: string): string {
  const v = process.env[key];
  return v ? v : fallback;
}

function envFlag(key: string): boolean {
  return process.env[key] === "1" || process.env[key] === "true";
}

/** Load a Solana keypair from a JSON byte array (never commit the file). */
function loadKeypair(json: string): Keypair {
  const secretKey = Uint8Array.from(JSON.parse(json) as number[]);
  return Keypair.fromSecretKey(secretKey);
}

// ---------------------------------------------------------------------------
// Borsh + Anchor instruction encoding
// ---------------------------------------------------------------------------

/**
 * Anchor instruction discriminator: `sha256("global:<name>")[0..8]`.
 * Mirrors how Anchor derives the 8-byte discriminator baked into every handler.
 */
function ixDiscriminator(name: string): Uint8Array {
  const hash = createHash("sha256").update(`global:${name}`).digest();
  return hash.subarray(0, 8);
}

function borshU8(v: number): Uint8Array {
  return Uint8Array.of(v & 0xff);
}

function borshU16(v: number): Uint8Array {
  const b = new Uint8Array(2);
  new DataView(b.buffer, b.byteOffset, b.byteLength).setUint16(0, v, true);
  return b;
}

function borshU64(v: bigint): Uint8Array {
  const b = new Uint8Array(8);
  new DataView(b.buffer, b.byteOffset, b.byteLength).setBigUint64(0, v, true);
  return b;
}

function borshPubkey(p: PublicKey): Uint8Array {
  return p.toBytes();
}

function concatBytes(...parts: Uint8Array[]): Buffer {
  return Buffer.concat(parts.map((p) => Buffer.from(p)));
}

export interface IxKey {
  pubkey: PublicKey;
  isSigner: boolean;
  isWritable: boolean;
}

/** Build an Anchor instruction: 8-byte discriminator + borsh-encoded args. */
function makeIx(
  programId: PublicKey,
  name: string,
  keys: IxKey[],
  args: Uint8Array[],
): TransactionInstruction {
  const discriminator = ixDiscriminator(name);
  return new TransactionInstruction({
    keys,
    programId,
    data: concatBytes(discriminator, ...args),
  });
}

// ---------------------------------------------------------------------------
// PDA derivation
// ---------------------------------------------------------------------------

export interface Pdas {
  market: PublicKey;
  marketBump: number;
  orderBook: PublicKey;
  orderBookBump: number;
  vault: PublicKey;
  vaultBump: number;
}

export function derivePdas(programId: PublicKey): Pdas {
  const [market, marketBump] = PublicKey.findProgramAddressSync([PERP_MARKET_SEED], programId);
  const [orderBook, orderBookBump] = PublicKey.findProgramAddressSync(
    [ORDER_BOOK_SEED, market.toBytes()],
    programId,
  );
  const [vault, vaultBump] = PublicKey.findProgramAddressSync([VAULT_SEED], programId);
  return { market, marketBump, orderBook, orderBookBump, vault, vaultBump };
}

export function userCollateralPda(programId: PublicKey, market: PublicKey, user: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(
    [USER_COLLATERAL_SEED, market.toBytes(), user.toBytes()],
    programId,
  )[0];
}

export function positionPda(
  programId: PublicKey,
  market: PublicKey,
  user: PublicKey,
  side: number,
): PublicKey {
  return PublicKey.findProgramAddressSync(
    [POSITION_SEED, market.toBytes(), user.toBytes(), Uint8Array.of(side)],
    programId,
  )[0];
}

/** Derive the associated token account for `mint` owned by `owner`. */
export function associatedTokenAccount(owner: PublicKey, mint: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(
    [owner.toBytes(), TOKEN_PROGRAM_ID.toBytes(), mint.toBytes()],
    ASSOCIATED_TOKEN_PROGRAM_ID,
  )[0];
}

// ---------------------------------------------------------------------------
// Account decoders (mirror programs/fructus/src/state.rs borsh layouts)
// ---------------------------------------------------------------------------

const DISCRIMINATOR_LEN = 8;

export interface MarketState {
  indexSource: PublicKey;
  collateralMint: PublicKey;
  fundingK: bigint;
  maxFunding: bigint;
  fundingEpochSlots: bigint;
  initialMarginBps: number;
  maintenanceMarginBps: number;
  authority: PublicKey;
  vault: PublicKey;
  fundingEpoch: bigint;
  indexN: bigint;
  indexD: bigint;
  fundingAccumulator: bigint;
  bump: number;
}

/** Decode a `PerpMarket` account (borsh payload after the 8-byte discriminator). */
export function decodeMarket(data: Buffer | null): MarketState | null {
  if (!data || data.length < DISCRIMINATOR_LEN + 197) return null;
  const o = DISCRIMINATOR_LEN;
  return {
    indexSource: new PublicKey(data.subarray(o, o + 32)),
    collateralMint: new PublicKey(data.subarray(o + 32, o + 64)),
    fundingK: data.readBigUInt64LE(o + 64),
    maxFunding: data.readBigUInt64LE(o + 72),
    fundingEpochSlots: data.readBigUInt64LE(o + 80),
    initialMarginBps: data.readUInt16LE(o + 88),
    maintenanceMarginBps: data.readUInt16LE(o + 90),
    authority: new PublicKey(data.subarray(o + 92, o + 124)),
    vault: new PublicKey(data.subarray(o + 124, o + 156)),
    fundingEpoch: data.readBigUInt64LE(o + 156),
    indexN: data.readBigUInt64LE(o + 164),
    indexD: data.readBigUInt64LE(o + 172),
    fundingAccumulator: readI128LE(data, o + 180),
    bump: data[o + 196],
  };
}

export interface CollateralState {
  deposited: bigint;
  reserved: bigint;
  bump: number;
}

/** Decode a `UserCollateral` account (deposited: u64, reserved: u64, bump: u8). */
export function decodeUserCollateral(data: Buffer | null): CollateralState | null {
  if (!data || data.length < DISCRIMINATOR_LEN + 17) return null;
  const o = DISCRIMINATOR_LEN;
  return {
    deposited: data.readBigUInt64LE(o),
    reserved: data.readBigUInt64LE(o + 8),
    bump: data[o + 16],
  };
}

export interface PositionState {
  market: PublicKey;
  owner: PublicKey;
  side: number;
  notional: bigint;
  entryNSum: bigint;
  entryDSum: bigint;
  collateral: bigint;
  lastFundingEpoch: bigint;
  closedNotional: bigint;
  openSlot: bigint;
  bump: number;
}

/** Decode a `Position` account (borsh payload after the 8-byte discriminator). */
export function decodePosition(data: Buffer | null): PositionState | null {
  if (!data || data.length < DISCRIMINATOR_LEN + 138) return null;
  const o = DISCRIMINATOR_LEN;
  return {
    market: new PublicKey(data.subarray(o, o + 32)),
    owner: new PublicKey(data.subarray(o + 32, o + 64)),
    side: data[o + 64],
    notional: data.readBigUInt64LE(o + 65),
    entryNSum: data.readBigUInt64LE(o + 73) | (data.readBigUInt64LE(o + 81) << 64n),
    entryDSum: data.readBigUInt64LE(o + 89) | (data.readBigUInt64LE(o + 97) << 64n),
    collateral: data.readBigUInt64LE(o + 105),
    lastFundingEpoch: data.readBigUInt64LE(o + 113),
    closedNotional: data.readBigUInt64LE(o + 121),
    openSlot: data.readBigUInt64LE(o + 129),
    bump: data[o + 137],
  };
}

/** Throw unless `state` decoded; used where the account must already exist. */
function mustCollateral(state: CollateralState | null, label: string): CollateralState {
  if (!state) throw new Error(`[e2e] ${label} UserCollateral account missing or too short`);
  return state;
}

function readI128LE(data: Buffer, offset: number): bigint {
  const lo = data.readBigUInt64LE(offset);
  const hi = data.readBigUInt64LE(offset + 8);
  let v = (hi << 64n) | lo;
  const signBit = 1n << 127n;
  if ((v & signBit) !== 0n) v -= 1n << 128n;
  return v;
}

// ---------------------------------------------------------------------------
// Pure funding engine mirror (R-F1..R-F3) — offline-verifiable sign convention
// ---------------------------------------------------------------------------

/** `mark - index` (signed, APY_SCALE fixed-point). */
export function premium(mark: bigint, index: bigint): bigint {
  return mark - index;
}

/** `clamp(fundingK·premium/APY_SCALE, -maxFunding, +maxFunding)` (signed). */
export function fundingRate(premium_: bigint, fundingK: bigint, maxFunding: bigint): bigint {
  const cap = maxFunding;
  const raw = fundingK * premium_;
  const unscaled = raw / APY_SCALE; // BigInt division truncates toward zero (i128-like)
  return unscaled < -cap ? -cap : unscaled > cap ? cap : unscaled;
}

export type SideFlow = "long" | "short";

/**
 * `notional·rate/APY_SCALE · epochs × side_flow` (signed).
 * R-F3 sign convention: `premium > 0 ⟹ long pays (negative flow), short
 * receives (positive flow)`, and the two are exact opposites.
 */
export function fundingPayment(
  notional: bigint,
  rate: bigint,
  epochs: bigint,
  side: SideFlow,
): bigint {
  const scaled = (notional * rate) / APY_SCALE;
  const flow = side === "long" ? -1n : 1n;
  return scaled * epochs * flow;
}

/** Assert R-F3: positive premium => long pays, short receives, exact opposites. */
export function assertFundingSignConvention(): void {
  const notional = 1_000_000_000n; // 1,000 USDC (6dp)
  const epochs = 1n;
  for (const premiumValue of [1n, 5_000n, 1_000_000n]) {
    const rate = fundingRate(premiumValue, 100_000n, 1_000_000n);
    const longPay = fundingPayment(notional, rate, epochs, "long");
    const shortPay = fundingPayment(notional, rate, epochs, "short");
    if (premiumValue > 0n) {
      // premium > 0 => rate >= 0; a positive premium must make longs pay.
      if (rate > 0n) {
        console.assert(longPay < 0n, "premium>0 => long flow must be negative");
        console.assert(shortPay > 0n, "premium>0 => short flow must be positive");
        console.assert(longPay === -shortPay, "long/short must be exact opposites");
      }
    }
  }
  // Explicit throw-based check so the dry-run fails loudly if the convention
  // is ever violated (console.assert is not fail-fast under Node's test runner).
  const longPay = fundingPayment(notional, fundingRate(5_000n, 500_000n, 1_000_000n), 1n, "long");
  const shortPay = fundingPayment(notional, fundingRate(5_000n, 500_000n, 1_000_000n), 1n, "short");
  if (!(longPay < 0n && shortPay > 0n && longPay === -shortPay)) {
    throw new Error(
      `R-F3 funding sign convention violated: long=${longPay} short=${shortPay}`,
    );
  }
  console.log("[sign] R-F3 convention OK: premium>0 => long pays, short receives (exact opposites)");
}

// ---------------------------------------------------------------------------
// Instruction builders (thin, best-effort; used only in the network run)
// ---------------------------------------------------------------------------

function initializeMarketIx(
  programId: PublicKey,
  keys: IxKey[],
  collateralMint: PublicKey,
  fundingK: number,
  maxFunding: number,
  fundingEpochSlots: number,
  initialMarginBps: number,
  maintenanceMarginBps: number,
): TransactionInstruction {
  return makeIx(programId, "initialize_market", keys, [
    borshPubkey(collateralMint),
    borshU64(BigInt(fundingK)),
    borshU64(BigInt(maxFunding)),
    borshU64(BigInt(fundingEpochSlots)),
    borshU16(initialMarginBps),
    borshU16(maintenanceMarginBps),
  ]);
}

function initializeOrderBookIx(programId: PublicKey, keys: IxKey[]): TransactionInstruction {
  return makeIx(programId, "initialize_order_book", keys, []);
}

function initializeCollateralVaultIx(
  programId: PublicKey,
  keys: IxKey[],
): TransactionInstruction {
  return makeIx(programId, "initialize_collateral_vault", keys, []);
}

function depositCollateralIx(
  programId: PublicKey,
  keys: IxKey[],
  amount: bigint,
): TransactionInstruction {
  return makeIx(programId, "deposit_collateral", keys, [borshU64(amount)]);
}

function openPositionIx(
  programId: PublicKey,
  keys: IxKey[],
  side: number,
  size: bigint,
  price: bigint,
): TransactionInstruction {
  return makeIx(programId, "open_position", keys, [
    borshU8(side),
    borshU64(size),
    borshU64(price),
  ]);
}

function closePositionIx(
  programId: PublicKey,
  keys: IxKey[],
  side: number,
  size: bigint,
): TransactionInstruction {
  return makeIx(programId, "close_position", keys, [borshU8(side), borshU64(size)]);
}

function settleFundingIx(programId: PublicKey, keys: IxKey[]): TransactionInstruction {
  return makeIx(programId, "settle_funding", keys, []);
}

function settleCloseIx(programId: PublicKey, keys: IxKey[]): TransactionInstruction {
  return makeIx(programId, "settle_close", keys, []);
}

function crankIx(programId: PublicKey, keys: IxKey[]): TransactionInstruction {
  return makeIx(programId, "crank", keys, []);
}

function placeLimitOrderIx(
  programId: PublicKey,
  keys: IxKey[],
  side: number,
  price: bigint,
  size: bigint,
): TransactionInstruction {
  return makeIx(programId, "place_limit_order", keys, [
    borshU8(side),
    borshU64(price),
    borshU64(size),
  ]);
}

function placeMarketOrderIx(
  programId: PublicKey,
  keys: IxKey[],
  side: number,
  size: bigint,
): TransactionInstruction {
  return makeIx(programId, "place_market_order", keys, [borshU8(side), borshU64(size)]);
}

// ---------------------------------------------------------------------------
// Network lifecycle walk (guarded by RUN_E2E / --network)
// ---------------------------------------------------------------------------

async function send(
  connection: Connection,
  ix: TransactionInstruction[],
  signers: Keypair[],
): Promise<string> {
  const tx = new Transaction().add(...ix);
  const sig = await sendAndConfirmTransaction(connection, tx, signers, {
    skipPreflight: false,
    preflightCommitment: "confirmed",
  });
  console.log(`  -> confirmed: ${sig}`);
  return sig;
}

async function networkWalk(): Promise<void> {
  const rpcUrl = env("RPC_URL");
  const programId = new PublicKey(env("PROGRAM_ID"));
  const authority = loadKeypair(env("AUTHORITY_KEYPAIR"));
  const longUser = loadKeypair(env("LONG_USER_KEYPAIR"));
  const shortUser = loadKeypair(env("SHORT_USER_KEYPAIR"));
  const indexSource = new PublicKey(env("INDEX_SOURCE"));
  const usdcMint = new PublicKey(env("USDC_MINT"));
  const fundingEpochSlots = Number(envOr("FUNDING_EPOCH_SLOTS", "16"));

  const connection = new Connection(rpcUrl, "confirmed");

  // Market parameters (divergence + margins) — configurable for a fast test.
  const fundingK = Number(envOr("FUNDING_K", "100000"));
  const maxFunding = Number(envOr("MAX_FUNDING", "1000000"));
  const initialMarginBps = Number(envOr("INITIAL_MARGIN_BPS", "1000"));
  const maintenanceMarginBps = Number(envOr("MAINTENANCE_MARGIN_BPS", "500"));
  const size = BigInt(envOr("POSITION_SIZE", "1000000")); // 1 USDC (6dp)
  const price = BigInt(envOr("PRICE", "1000000")); // 1.0 APY_SCALE

  const pdas = derivePdas(programId);

  console.log(`[e2e] RPC: ${rpcUrl}`);
  console.log(`[e2e] program: ${programId.toString()}`);
  console.log(`[e2e] market PDA: ${pdas.market.toString()} (bump ${pdas.marketBump})`);
  console.log(`[e2e] order book PDA: ${pdas.orderBook.toString()}`);
  console.log(`[e2e] vault PDA: ${pdas.vault.toString()}`);

  // --- 1. initialize market (idempotent: skip if already present) -----------
  const marketInfo = await connection.getAccountInfo(pdas.market);
  if (!marketInfo) {
    console.log("[e2e] initializing market...");
    const ix = initializeMarketIx(
      programId,
      [
        { pubkey: pdas.market, isSigner: false, isWritable: true },
        { pubkey: indexSource, isSigner: false, isWritable: false },
        { pubkey: authority.publicKey, isSigner: true, isWritable: false },
        { pubkey: authority.publicKey, isSigner: true, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
      ],
      usdcMint,
      fundingK,
      maxFunding,
      fundingEpochSlots,
      initialMarginBps,
      maintenanceMarginBps,
    );
    await send(connection, [ix], [authority]);
  } else {
    console.log("[e2e] market already present; loading params");
  }

  const market = decodeMarket((await connection.getAccountInfo(pdas.market))?.data ?? null);
  if (!market) throw new Error("could not decode PerpMarket account");
  console.log(
    `[e2e] market: funding_epoch_slots=${market.fundingEpochSlots} funding_k=${market.fundingK} max_funding=${market.maxFunding}`,
  );

  // --- 2. initialize order book (idempotent) --------------------------------
  if (!(await connection.getAccountInfo(pdas.orderBook))) {
    console.log("[e2e] initializing order book...");
    await send(
      connection,
      [
        initializeOrderBookIx(programId, [
          { pubkey: pdas.orderBook, isSigner: false, isWritable: true },
          { pubkey: pdas.market, isSigner: false, isWritable: false },
          { pubkey: authority.publicKey, isSigner: true, isWritable: false },
          { pubkey: authority.publicKey, isSigner: true, isWritable: true },
          { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
        ]),
      ],
      [authority],
    );
  }

  // --- 3. initialize the collateral vault (idempotent) ----------------------
  const vaultInfo = await connection.getAccountInfo(pdas.vault);
  if (!vaultInfo || vaultInfo.data.length === 0) {
    console.log("[e2e] initializing collateral vault...");
    await send(
      connection,
      [
        initializeCollateralVaultIx(programId, [
          { pubkey: pdas.market, isSigner: false, isWritable: false },
          { pubkey: authority.publicKey, isSigner: true, isWritable: false },
          { pubkey: authority.publicKey, isSigner: true, isWritable: true },
          { pubkey: pdas.vault, isSigner: false, isWritable: true },
          { pubkey: usdcMint, isSigner: false, isWritable: false },
          { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
        ]),
      ],
      [authority],
    );
  }

  // --- 4. deposit collateral for both users ---------------------------------
  const depositAmount = BigInt(envOr("DEPOSIT_AMOUNT", "1000000000")); // 1,000 USDC
  for (const user of [longUser, shortUser]) {
    const label = user === longUser ? "long" : "short";
    const ata = associatedTokenAccount(user.publicKey, usdcMint);
    const ataInfo = await connection.getAccountInfo(ata);
    if (!ataInfo) {
      // Create the ATA via the associated-token-account instruction.
      const createIx = new TransactionInstruction({
        programId: ASSOCIATED_TOKEN_PROGRAM_ID,
        keys: [
          { pubkey: user.publicKey, isSigner: true, isWritable: true },
          { pubkey: ata, isSigner: false, isWritable: true },
          { pubkey: user.publicKey, isSigner: false, isWritable: false },
          { pubkey: usdcMint, isSigner: false, isWritable: false },
          { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
        ],
        data: Buffer.alloc(0),
      });
      await send(connection, [createIx], [user]);
    }
    const balance = await connection.getTokenAccountBalance(ata).catch(() => null);
    if (!balance || BigInt(balance.value.amount) < depositAmount) {
      throw new Error(
        `[e2e] ${label} user ATA ${ata.toString()} holds < ${depositAmount} USDC — fund it on devnet first`,
      );
    }
    const uc = userCollateralPda(programId, pdas.market, user.publicKey);
    console.log(`[e2e] depositing ${depositAmount} for ${label} user...`);
    await send(
      connection,
      [
        depositCollateralIx(programId, [
          { pubkey: user.publicKey, isSigner: true, isWritable: true },
          { pubkey: pdas.market, isSigner: false, isWritable: false },
          { pubkey: uc, isSigner: false, isWritable: true },
          { pubkey: pdas.vault, isSigner: false, isWritable: true },
          { pubkey: ata, isSigner: false, isWritable: true },
          { pubkey: usdcMint, isSigner: false, isWritable: false },
          { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
        ], depositAmount),
      ],
      [user],
    );
  }

  // --- 5. open a full LONG and a full SHORT --------------------------------
  // shortUser posts a resting ask; the long takers fill against it (long user
  // opens LONG as taker). The long user then posts a resting bid; the short
  // user opens SHORT as taker against it. Neither maker side is settled, so we
  // are left with exactly one LONG (long user) and one SHORT (short user).
  const longPos = positionPda(programId, pdas.market, longUser.publicKey, SIDE_LONG);
  const shortPos = positionPda(programId, pdas.market, shortUser.publicKey, SIDE_SHORT);

  console.log("[e2e] short user posts a resting ask (maker for the long)...");
  await send(
    connection,
    [
      placeLimitOrderIx(programId, [
        { pubkey: pdas.orderBook, isSigner: false, isWritable: true },
        { pubkey: pdas.market, isSigner: false, isWritable: false },
        { pubkey: indexSource, isSigner: false, isWritable: false },
        { pubkey: shortUser.publicKey, isSigner: true, isWritable: false },
      ], SIDE_SHORT, price, size),
    ],
    [shortUser],
  );

  console.log("[e2e] long user opens LONG (market taker)...");
  await send(
    connection,
    [
      openPositionIx(programId, [
        { pubkey: longUser.publicKey, isSigner: true, isWritable: true },
        { pubkey: pdas.market, isSigner: false, isWritable: false },
        { pubkey: pdas.orderBook, isSigner: false, isWritable: true },
        { pubkey: indexSource, isSigner: false, isWritable: false },
        { pubkey: longPos, isSigner: false, isWritable: true },
        { pubkey: userCollateralPda(programId, pdas.market, longUser.publicKey), isSigner: false, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
      ], SIDE_LONG, size, 0n),
    ],
    [longUser],
  );

  console.log("[e2e] long user posts a resting bid (maker for the short)...");
  await send(
    connection,
    [
      placeLimitOrderIx(programId, [
        { pubkey: pdas.orderBook, isSigner: false, isWritable: true },
        { pubkey: pdas.market, isSigner: false, isWritable: false },
        { pubkey: indexSource, isSigner: false, isWritable: false },
        { pubkey: longUser.publicKey, isSigner: true, isWritable: false },
      ], SIDE_LONG, price, size),
    ],
    [longUser],
  );

  console.log("[e2e] short user opens SHORT (market taker)...");
  await send(
    connection,
    [
      openPositionIx(programId, [
        { pubkey: shortUser.publicKey, isSigner: true, isWritable: true },
        { pubkey: pdas.market, isSigner: false, isWritable: false },
        { pubkey: pdas.orderBook, isSigner: false, isWritable: true },
        { pubkey: indexSource, isSigner: false, isWritable: false },
        { pubkey: shortPos, isSigner: false, isWritable: true },
        { pubkey: userCollateralPda(programId, pdas.market, shortUser.publicKey), isSigner: false, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
      ], SIDE_SHORT, size, 0n),
    ],
    [shortUser],
  );

  // --- 6. advance funding (crank + settle_funding for each position) --------
  console.log("[e2e] cranking the event queue (any residual fills)...");
  await send(
    connection,
    [
      crankIx(programId, [
        { pubkey: pdas.orderBook, isSigner: false, isWritable: true },
        { pubkey: pdas.market, isSigner: false, isWritable: false },
        { pubkey: indexSource, isSigner: false, isWritable: false },
        { pubkey: authority.publicKey, isSigner: true, isWritable: false },
      ]),
    ],
    [authority],
  );

  const longBefore = mustCollateral(
    decodeUserCollateral(
      (await connection.getAccountInfo(userCollateralPda(programId, pdas.market, longUser.publicKey)))?.data ?? null,
    ),
    "long",
  );
  const shortBefore = mustCollateral(
    decodeUserCollateral(
      (await connection.getAccountInfo(userCollateralPda(programId, pdas.market, shortUser.publicKey)))?.data ?? null,
    ),
    "short",
  );

  console.log("[e2e] settling funding for the LONG position...");
  await send(
    connection,
    [
      settleFundingIx(programId, [
        { pubkey: pdas.market, isSigner: false, isWritable: true },
        { pubkey: longPos, isSigner: false, isWritable: true },
        { pubkey: userCollateralPda(programId, pdas.market, longUser.publicKey), isSigner: false, isWritable: true },
        { pubkey: pdas.orderBook, isSigner: false, isWritable: false },
        { pubkey: indexSource, isSigner: false, isWritable: false },
      ]),
    ],
    [authority],
  );

  console.log("[e2e] settling funding for the SHORT position...");
  await send(
    connection,
    [
      settleFundingIx(programId, [
        { pubkey: pdas.market, isSigner: false, isWritable: true },
        { pubkey: shortPos, isSigner: false, isWritable: true },
        { pubkey: userCollateralPda(programId, pdas.market, shortUser.publicKey), isSigner: false, isWritable: true },
        { pubkey: pdas.orderBook, isSigner: false, isWritable: false },
        { pubkey: indexSource, isSigner: false, isWritable: false },
      ]),
    ],
    [authority],
  );

  const longAfter = mustCollateral(
    decodeUserCollateral(
      (await connection.getAccountInfo(userCollateralPda(programId, pdas.market, longUser.publicKey)))?.data ?? null,
    ),
    "long",
  );
  const shortAfter = mustCollateral(
    decodeUserCollateral(
      (await connection.getAccountInfo(userCollateralPda(programId, pdas.market, shortUser.publicKey)))?.data ?? null,
    ),
    "short",
  );

  const longDelta = longAfter.deposited - longBefore.deposited;
  const shortDelta = shortAfter.deposited - shortBefore.deposited;

  // Read the mark (order-book mid) + index for the sign-convention assertion.
  const marketAfter = decodeMarket((await connection.getAccountInfo(pdas.market))?.data ?? null);
  const index =
    marketAfter && marketAfter.indexD !== 0n ? marketAfter.indexN / marketAfter.indexD : 0n;
  console.log(`[e2e] funding settled: long Δ=${longDelta}, short Δ=${shortDelta}, index=${index}`);

  // Best-effort sign-convention check: only meaningful when premium != 0.
  if (longDelta !== 0n || shortDelta !== 0n) {
    if (longDelta < 0n && shortDelta > 0n && longDelta === -shortDelta) {
      console.log("[sign] R-F3 confirmed on-chain: premium>0 => long pays, short receives (exact opposites)");
    } else if (shortDelta < 0n && longDelta > 0n && longDelta === -shortDelta) {
      console.log("[sign] R-F3 confirmed on-chain (negative premium): short pays, long receives");
    } else {
      console.log("[sign] note: on-chain premium near zero, no funding flowed (skipped)");
    }
  } else {
    console.log("[sign] note: no funding flowed (premium==0 or same-epoch re-settle)");
  }

  // --- 7. close each position + settle_close --------------------------------
  console.log("[e2e] closing the LONG (market-IOC on the ask side)...");
  await send(
    connection,
    [
      closePositionIx(programId, [
        { pubkey: longUser.publicKey, isSigner: true, isWritable: false },
        { pubkey: pdas.market, isSigner: false, isWritable: false },
        { pubkey: pdas.orderBook, isSigner: false, isWritable: true },
        { pubkey: indexSource, isSigner: false, isWritable: false },
        { pubkey: longPos, isSigner: false, isWritable: true },
        { pubkey: userCollateralPda(programId, pdas.market, longUser.publicKey), isSigner: false, isWritable: true },
      ], SIDE_LONG, size),
    ],
    [longUser],
  );

  console.log("[e2e] closing the SHORT (market-IOC on the bid side)...");
  await send(
    connection,
    [
      closePositionIx(programId, [
        { pubkey: shortUser.publicKey, isSigner: true, isWritable: false },
        { pubkey: pdas.market, isSigner: false, isWritable: false },
        { pubkey: pdas.orderBook, isSigner: false, isWritable: true },
        { pubkey: indexSource, isSigner: false, isWritable: false },
        { pubkey: shortPos, isSigner: false, isWritable: true },
        { pubkey: userCollateralPda(programId, pdas.market, shortUser.publicKey), isSigner: false, isWritable: true },
      ], SIDE_SHORT, size),
    ],
    [shortUser],
  );

  for (const [label, pos] of [
    ["long", longPos],
    ["short", shortPos],
  ] as const) {
    await send(
      connection,
      [
        settleCloseIx(programId, [
          { pubkey: pdas.market, isSigner: false, isWritable: false },
          { pubkey: pos, isSigner: false, isWritable: true },
          { pubkey: userCollateralPda(programId, pdas.market, label === "long" ? longUser.publicKey : shortUser.publicKey), isSigner: false, isWritable: true },
          { pubkey: indexSource, isSigner: false, isWritable: false },
        ]),
      ],
      [authority],
    );
  }

  console.log("[e2e] lifecycle walk complete");
}

// ---------------------------------------------------------------------------
// Offline dry-run (default): no network; derive PDAs + assert the convention.
// ---------------------------------------------------------------------------

function offlineDryRun(programId: PublicKey): void {
  console.log("[dry-run] PROGRAM_ID:", programId.toString());
  const pdas = derivePdas(programId);
  console.log("[dry-run] perp_market PDA :", pdas.market.toString(), `(bump ${pdas.marketBump})`);
  console.log("[dry-run] order_book PDA  :", pdas.orderBook.toString(), `(bump ${pdas.orderBookBump})`);
  console.log("[dry-run] vault PDA       :", pdas.vault.toString(), `(bump ${pdas.vaultBump})`);

  console.log("\n[outline] planned devnet lifecycle walk:");
  console.log("  1. initialize_market(index_source, usdc_mint, funding_k, max_funding, epoch_slots, im, mm)");
  console.log("  2. initialize_order_book()");
  console.log("  3. initialize_collateral_vault()");
  console.log("  4. deposit_collateral(<amount>) for the long user + the short user");
  console.log("  5. short posts a resting ask -> long opens LONG (taker); long posts a resting bid -> short opens SHORT (taker)");
  console.log("  6. crank(); settle_funding(long); settle_funding(short)");
  console.log("  7. close_position(long); close_position(short); settle_close(long); settle_close(short)");

  assertFundingSignConvention();

  console.log("\n[dry-run] offline check OK. Set RUN_E2E=1 (or pass --network) plus the docs env to run it.");
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

function main(): void {
  const programId = new PublicKey(envOr("PROGRAM_ID", DEFAULT_PROGRAM_ID));
  const wantNetwork = envFlag("RUN_E2E") || process.argv.slice(2).includes("--network");

  if (wantNetwork) {
    networkWalk().catch((err) => {
      console.error("\n[e2e] FAILED:", err);
      process.exit(1);
    });
  } else {
    offlineDryRun(programId);
  }
}

main();
