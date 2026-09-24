//! Transaction v1 (SIMD-0385) opt-in send path — additive to the v0/legacy helpers in
//! `instructions.ts`. Traders opt in explicitly (`--tx-version v1` in the CLI); every
//! existing path keeps v0/legacy semantics by default.
//!
//! Layout of responsibilities:
//!   - gate:     `TXV1_GATE_ID` + `isV1GateActive` (fail closed when inactive)
//!   - limits:   `roundUpToPage` + `limitsFromSimulation` (explicit, page-aligned values —
//!               v1 budgets ZERO for any unset limit, so both limits must always be set)
//!   - build:    `buildV1Message` (rejects ComputeBudget instructions, deduplicates static
//!               addresses, enforces the 4096-byte message limit)
//!   - estimate: `estimateV1Limits` (one simulation with both limits maxed, kit estimator)
//!   - send:     `sendV1Instructions` (gate check first, then build → estimate → sign →
//!               base64 send; optional confirmation via the web3 connection)
//!
//! Issue #19: implemented. All @solana/kit calls live in this file; web3.js
//! `TransactionInstruction`/`Keypair` objects stay the SDK's public currency — the conversion
//! to kit instructions and kit signers happens here and nowhere else. The v1 knobs the web3
//! `Connection` cannot express (resource limits, priority fee, the v1 message shape) travel
//! through kit's message config, so no `rpcSubscriptions` is ever needed: the send goes to
//! `connection.rpcEndpoint` and, when requested, confirmation polls through web3.

import { PublicKey } from "@solana/web3.js";
import type { Connection, Keypair, TransactionInstruction } from "@solana/web3.js";
import {
  AccountRole,
  addSignersToTransactionMessage,
  address,
  appendTransactionMessageInstructions,
  compileTransactionMessage,
  createKeyPairSignerFromBytes,
  createSolanaRpc,
  createTransactionMessage,
  estimateResourceLimitsFactory,
  fillTransactionMessageProvisoryResourceLimits,
  getBase64EncodedWireTransaction,
  getSignatureFromTransaction,
  getTransactionMessageSize,
  setTransactionMessageConfig,
  setTransactionMessageFeePayer,
  setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
} from "@solana/kit";
import type {
  AccountMeta,
  Blockhash,
  Instruction,
  TransactionMessage,
  TransactionMessageWithFeePayer,
  TransactionMessageWithLifetime,
} from "@solana/kit";

/** A v1 transaction message with its fee payer and lifetime already set. */
export type V1Message = Extract<TransactionMessage, { version: 1 }> &
  TransactionMessageWithFeePayer &
  TransactionMessageWithLifetime;

/** The v1 feature gate (SIMD-0385). Feature accounts live at the feature id's address. */
export const TXV1_GATE_ID = "txv1aq4pp281K9um3tnPgkfX8UqtFT6wcVW3hNezGLL";

/** v1 resource limits are page-granular: the loaded-accounts data size rounds up to 32 KiB. */
export const V1_PAGE_BYTES = 32768;

/** Serialized v1 message size limit (bytes). */
export const V1_MAX_MESSAGE_BYTES = 4096;

/** The ComputeBudget program. No-op under v1: its settings travel in the message config. */
const COMPUTE_BUDGET_PROGRAM_ID = "ComputeBudget111111111111111111111111111111";

/** Raised when the v1 path is used while the feature gate is inactive. */
export class V1UnavailableError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "V1UnavailableError";
  }
}

/** Explicit resource limits for a v1 transaction message. */
export interface V1Limits {
  computeUnitLimit: number;
  loadedAccountsDataSizeLimit: number;
}

/** The two numbers read from one maxed-limits simulation. */
export interface V1SimulationNumbers {
  unitsConsumed: number;
  loadedAccountsDataSize: number;
}

/** Input for the v1 message builder. */
export interface V1BuildInput {
  instructions: TransactionInstruction[];
  feePayer: PublicKey;
  lifetime: { blockhash: string; lastValidBlockHeight: bigint };
}

/**
 * Convert a web3.js instruction into the kit instruction a v1 message carries. v1 messages
 * have no address-lookup-table support, so every meta is a plain `AccountMeta` (no
 * `lookupTableAddress`); the signer/writable flags map 1:1 onto kit's `AccountRole`.
 */
function toKitInstruction(instruction: TransactionInstruction): Instruction<string, readonly AccountMeta[]> {
  return {
    programAddress: address(instruction.programId.toBase58()),
    accounts: instruction.keys.map((key) => ({
      address: address(key.pubkey.toBase58()),
      role: key.isSigner
        ? key.isWritable
          ? AccountRole.WRITABLE_SIGNER
          : AccountRole.READONLY_SIGNER
        : key.isWritable
          ? AccountRole.WRITABLE
          : AccountRole.READONLY,
    })),
    data: new Uint8Array(instruction.data),
  };
}

/** Round a byte count up to the next `V1_PAGE_BYTES` multiple; 0 stays 0. */
export function roundUpToPage(bytes: number): number {
  if (bytes <= 0) {
    return 0;
  }
  return Math.ceil(bytes / V1_PAGE_BYTES) * V1_PAGE_BYTES;
}

/** Derive explicit, page-aligned limits from one simulation's numbers. */
export function limitsFromSimulation(sim: V1SimulationNumbers): V1Limits {
  return {
    computeUnitLimit: sim.unitsConsumed,
    loadedAccountsDataSizeLimit: roundUpToPage(sim.loadedAccountsDataSize),
  };
}

/** True when the v1 feature gate is active on the cluster behind `connection`. */
export async function isV1GateActive(connection: Connection): Promise<boolean> {
  const featureAccount = await connection.getAccountInfo(new PublicKey(TXV1_GATE_ID));
  return featureAccount !== null && featureAccount !== undefined;
}

/**
 * Build a kit v1 transaction message. See the header for the rejection rules.
 *
 * Duplicate references merge instead of throwing: kit's compiler keys its address map by
 * address and unions the roles, so re-referencing an account (two transfers sharing both
 * parties) compiles to a single entry. The post-compile guard below pins that outcome rather
 * than trusting it — a v1 message with a duplicate static address is rejected by the protocol.
 */
export function buildV1Message(input: V1BuildInput): V1Message {
  const kitInstructions: Instruction<string, readonly AccountMeta[]>[] = [];
  for (const instruction of input.instructions) {
    const programAddress = instruction.programId.toBase58();
    if (programAddress === COMPUTE_BUDGET_PROGRAM_ID) {
      // ComputeBudget instructions are no-ops in v1; dropping them silently would mislead.
      throw new Error(
        `v1: ComputeBudget instructions are not allowed in a v1 message (found ${programAddress}) — ` +
          "set the compute unit limit and the priority fee through the v1 message config instead",
      );
    }
    kitInstructions.push(toKitInstruction(instruction));
  }

  const withInstructions = appendTransactionMessageInstructions(
    kitInstructions,
    createTransactionMessage({ version: 1 }),
  );
  const withFeePayer = setTransactionMessageFeePayer(address(input.feePayer.toBase58()), withInstructions);
  const message = setTransactionMessageLifetimeUsingBlockhash(
    {
      blockhash: input.lifetime.blockhash as Blockhash,
      lastValidBlockHeight: BigInt(input.lifetime.lastValidBlockHeight),
    },
    withFeePayer,
  );

  const compiled = compileTransactionMessage(message);
  const staticAccounts = compiled.staticAccounts as readonly string[];
  if (new Set(staticAccounts).size !== staticAccounts.length) {
    throw new Error(
      "v1: the compiled v1 message contains a duplicate static address — v1 requires a deduplicated " +
        `address list (got ${staticAccounts.join(", ")})`,
    );
  }

  // The v1 size limit applies to the compiled, serialized message; kit measures exactly that.
  const serializedBytes = getTransactionMessageSize(message);
  if (serializedBytes > V1_MAX_MESSAGE_BYTES) {
    throw new Error(
      `v1: the compiled v1 message serializes to ${serializedBytes} bytes — over the ${V1_MAX_MESSAGE_BYTES}-byte limit`,
    );
  }

  // Provisory limits (0) so the estimator has a value to replace and the config mask is stable.
  return fillTransactionMessageProvisoryResourceLimits(message);
}

/**
 * Estimate the explicit limits for `message` (one simulation, both limits maxed).
 *
 * kit's estimator maxes both limits before simulating (CU 1,400,000 and loaded accounts data
 * 64 MiB), asks the RPC to replace the blockhash, and returns the measured numbers; the 32 KiB
 * page rounding the estimator does not apply is added here by `limitsFromSimulation`.
 */
export async function estimateV1Limits(connection: Connection, message: V1Message): Promise<V1Limits> {
  const rpc = createSolanaRpc(connection.rpcEndpoint);
  const estimateResourceLimits = estimateResourceLimitsFactory({ rpc });
  const estimate = await estimateResourceLimits(message);
  return limitsFromSimulation({
    unitsConsumed: estimate.computeUnitLimit,
    loadedAccountsDataSize: estimate.loadedAccountsDataSizeLimit,
  });
}

/**
 * Submit `instructions` as a v1 transaction. Gate check first (fail closed), then
 * build → estimate → configure → sign → base64 send. Returns the signature; with
 * `confirm: true` waits for confirmation through the web3 connection first.
 */
export async function sendV1Instructions(
  connection: Connection,
  instructions: TransactionInstruction[],
  signers: Keypair[],
  options?: { feePayer?: PublicKey; priorityFeeLamports?: bigint; confirm?: boolean },
): Promise<string> {
  // 1. Gate check FIRST: fail closed before anything is built, and touch nothing but
  //    `getAccountInfo` on the way out.
  if (!(await isV1GateActive(connection))) {
    throw new V1UnavailableError(
      `v1: transaction v1 is unavailable — the SIMD-0385 feature gate account ${TXV1_GATE_ID} ` +
        "does not exist on this cluster",
    );
  }

  const feePayer = options?.feePayer ?? signers[0]?.publicKey;
  if (!feePayer) {
    throw new Error(
      "v1: sendV1Instructions needs a fee payer — pass `options.feePayer` or at least one Keypair in `signers`",
    );
  }

  // 2. build → estimate → configure. Both limits are always written: unset v1 limits budget ZERO.
  const { blockhash, lastValidBlockHeight } = await connection.getLatestBlockhash();
  let message = buildV1Message({
    instructions,
    feePayer,
    lifetime: { blockhash, lastValidBlockHeight: BigInt(lastValidBlockHeight) },
  });
  const limits = await estimateV1Limits(connection, message);
  message = setTransactionMessageConfig(
    {
      computeUnitLimit: limits.computeUnitLimit,
      loadedAccountsDataSizeLimit: limits.loadedAccountsDataSizeLimit,
      ...(options?.priorityFeeLamports === undefined
        ? {}
        : { priorityFeeLamports: options.priorityFeeLamports }),
    },
    message,
  );

  // 3. Sign: web3 Keypair.secretKey (64 bytes) → kit signer; the fee payer signer is picked
  //    out of the list by address, so it must be one of the supplied signers.
  const kitSigners = await Promise.all(
    signers.map((signer) => createKeyPairSignerFromBytes(signer.secretKey)),
  );
  const signedTransaction = await signTransactionMessageWithSigners(
    addSignersToTransactionMessage(kitSigners, message),
  );
  const signature = getSignatureFromTransaction(signedTransaction);

  // 4. Send the base64 wire transaction (no rpcSubscriptions: the web3 Connection has no ws).
  const rpc = createSolanaRpc(connection.rpcEndpoint);
  await rpc.sendTransaction(getBase64EncodedWireTransaction(signedTransaction), { encoding: "base64" }).send();

  // 5. Optional confirmation through the web3 connection's blockheight strategy.
  if (options?.confirm) {
    const confirmation = await connection.confirmTransaction(
      { signature, blockhash, lastValidBlockHeight },
      "confirmed",
    );
    if (confirmation.value.err) {
      throw new Error(
        `v1: transaction ${signature} failed on-chain — ${JSON.stringify(confirmation.value.err)}`,
      );
    }
  }

  return signature;
}
