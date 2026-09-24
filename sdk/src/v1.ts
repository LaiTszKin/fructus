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
//! RED BASELINE (issue #19): this file carries the real signatures with stub behaviour only.
//! The implementer replaces the stubs; the acceptance tests in `test/v1-*.test.ts` are
//! frozen and must not be edited.

import type { Connection, Keypair, PublicKey, TransactionInstruction } from "@solana/web3.js";
import type {
  TransactionMessage,
  TransactionMessageWithFeePayer,
  TransactionMessageWithLifetime,
} from "@solana/kit";

/** A v1 transaction message with its fee payer and lifetime already set. */
export type V1Message = Extract<TransactionMessage, { version: 1 }> &
  TransactionMessageWithFeePayer &
  TransactionMessageWithLifetime;

/** The v1 feature gate (SIMD-0385). Feature accounts live at the feature id's address. */
export const TXV1_GATE_ID = "txv1aq4pp281K9um3pgkfX8UqtFT6wcVW3hNezGLL";

/** v1 resource limits are page-granular: the loaded-accounts data size rounds up to 32 KiB. */
export const V1_PAGE_BYTES = 32768;

/** Serialized v1 message size limit (bytes). */
export const V1_MAX_MESSAGE_BYTES = 4096;

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

/** Round a byte count up to the next `V1_PAGE_BYTES` multiple; 0 stays 0. */
export function roundUpToPage(_bytes: number): number {
  return 0;
}

/** Derive explicit, page-aligned limits from one simulation's numbers. */
export function limitsFromSimulation(_sim: V1SimulationNumbers): V1Limits {
  return { computeUnitLimit: 0, loadedAccountsDataSizeLimit: 0 };
}

/** True when the v1 feature gate is active on the cluster behind `connection`. */
export async function isV1GateActive(_connection: Connection): Promise<boolean> {
  return false;
}

/** Build a kit v1 transaction message. See the header for the rejection rules. */
export function buildV1Message(_input: V1BuildInput): V1Message {
  throw new Error("v1: buildV1Message not implemented (red baseline, issue #19)");
}

/** Estimate the explicit limits for `message` (one simulation, both limits maxed). */
export async function estimateV1Limits(
  _connection: Connection,
  _message: V1Message,
): Promise<V1Limits> {
  throw new Error("v1: estimateV1Limits not implemented (red baseline, issue #19)");
}

/**
 * Submit `instructions` as a v1 transaction. Gate check first (fail closed), then
 * build → estimate → configure → sign → base64 send. Returns the signature; with
 * `confirm: true` waits for confirmation through the web3 connection first.
 */
export async function sendV1Instructions(
  _connection: Connection,
  _instructions: TransactionInstruction[],
  _signers: Keypair[],
  _options?: { feePayer?: PublicKey; priorityFeeLamports?: bigint; confirm?: boolean },
): Promise<string> {
  throw new Error("v1: sendV1Instructions not implemented (red baseline, issue #19)");
}
