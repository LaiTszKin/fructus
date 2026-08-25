//! Shared helpers for the command builders.

import { Connection, PublicKey, Transaction, TransactionInstruction } from "@solana/web3.js";
import type { TraderConfig } from "../env.js";
import { PLACEHOLDER_PUBKEY } from "../env.js";
import { die } from "../errors.js";
import type { DerivedStep, DryRunReport, ValueStep } from "../report.js";
import { submitInstruction, submitTransaction } from "fructus-sdk/src/index.js";

export interface ReportParts {
  derived: DerivedStep[];
  values: ValueStep[];
  warnings: string[];
}

export function emptyParts(): ReportParts {
  return { derived: [], values: [], warnings: [] };
}

export function derived(label: string, address: PublicKey, seed: string): DerivedStep {
  return { label, address, seed };
}

export function value(label: string, v: string | bigint, note?: string): ValueStep {
  return { label, value: v.toString(), note };
}

/** Warn once for any placeholder pubkey that would be unsafe to submit. */
export function placeholders(cfg: TraderConfig, out: ReportParts): void {
  if (cfg.owner.equals(PLACEHOLDER_PUBKEY)) {
    out.warnings.push("owner is the placeholder pubkey (no keypair set)");
  }
  if (cfg.indexSource.equals(PLACEHOLDER_PUBKEY)) {
    out.warnings.push("INDEX_SOURCE/-index-source not set; using placeholder pubkey");
  }
  if (cfg.collateralMint.equals(PLACEHOLDER_PUBKEY)) {
    out.warnings.push("COLLATERAL_MINT/-mint not set; using placeholder pubkey");
  }
}

/**
 * Sign + submit a report's instructions via the SDK's submit helpers and return
 * the transaction signatures. Requires a real keypair + RPC URL.
 */
export async function submitReport(
  cfg: TraderConfig,
  report: DryRunReport,
): Promise<string[]> {
  if (report.instructions.length === 0) {
    return [];
  }
  if (!cfg.rpcUrl) {
    die("RPC_URL is not set; cannot submit");
  }
  if (!cfg.keypair) {
    die("a keypair is required to submit (set TRADER_KEYPAIR or --keypair)");
  }
  if (!cfg.owner || !cfg.keypair) {
    die("a keypair is required to submit");
  }
  const connection = new Connection(cfg.rpcUrl, "confirmed");
  const signers = [cfg.keypair];

  if (report.instructions.length === 1) {
    const i = report.instructions[0];
    const ix = new TransactionInstruction({
      programId: i.programId,
      keys: i.keys,
      data: i.data,
    });
    const sig = await submitInstruction(connection, ix, signers, cfg.owner);
    return [sig];
  }

  const tx = new Transaction().add(
    ...report.instructions.map((i) => ({
      programId: i.programId,
      keys: i.keys,
      data: i.data,
    })),
  );
  const sig = await submitTransaction(connection, tx, signers);
  return [sig];
}
