//! Offline/dry-run report model + renderer (R-C1).

import type { PublicKey, TransactionInstruction } from "@solana/web3.js";

export interface InstructionStep {
  name: string;
  programId: PublicKey;
  keys: TransactionInstruction["keys"];
  data: Buffer;
}

export interface DerivedStep {
  label: string;
  address: PublicKey;
  seed: string;
}

export interface ValueStep {
  label: string;
  value: string;
  note?: string;
}

export interface DryRunReport {
  command: string;
  mode: "offline" | "network";
  programId: PublicKey;
  market: PublicKey;
  owner: PublicKey;
  instructions: InstructionStep[];
  derived: DerivedStep[];
  values: ValueStep[];
  warnings: string[];
}

/** Convert a built `TransactionInstruction` into an `InstructionStep`. */
export function instructionStep(name: string, ix: TransactionInstruction): InstructionStep {
  return { name, programId: ix.programId, keys: ix.keys, data: ix.data };
}

const WRITE = "w";
const READ = "r";
const SIGNER = "signer";
const NON_SIGNER = "";

function keyTag(k: TransactionInstruction["keys"][number]): string {
  return `${k.isWritable ? WRITE : READ}/${k.isSigner ? SIGNER : NON_SIGNER}`;
}

/** Render a dry-run report to a string. */
export function renderReport(r: DryRunReport): string {
  const lines: string[] = [];
  lines.push(`=== fructus-cli ${r.command} ===`);
  lines.push(`mode: ${r.mode}${r.mode === "offline" ? " (dry-run; use --submit to post on-chain)" : ""}`);
  lines.push(`owner : ${r.owner.toBase58()}`);
  lines.push(`program: ${r.programId.toBase58()}`);
  lines.push(`market : ${r.market.toBase58()}`);

  if (r.derived.length > 0) {
    lines.push("derived:");
    for (const d of r.derived) {
      lines.push(`  ${d.label.padEnd(20)} ${d.address.toBase58()}  [${d.seed}]`);
    }
  }

  if (r.instructions.length > 0) {
    lines.push("instructions:");
    for (const ix of r.instructions) {
      lines.push(`  ${ix.name}  (program ${ix.programId.toBase58()})`);
      lines.push(`    data: 0x${ix.data.toString("hex")}`);
      for (const k of ix.keys) {
        lines.push(
          `    ${keyTag(k).padEnd(9)} ${k.pubkey.toBase58()}`,
        );
      }
    }
  }

  if (r.values.length > 0) {
    lines.push("values:");
    for (const v of r.values) {
      const line = `  ${v.label.padEnd(24)} ${v.value}`;
      lines.push(v.note ? `${line}  (${v.note})` : line);
    }
  }

  for (const w of r.warnings) {
    lines.push(`warning: ${w}`);
  }

  return lines.join("\n");
}

/** A minimal I/O bundle so CLI output is capturable in tests. */
export interface Io {
  out: (s: string) => void;
  err: (s: string) => void;
}

export const defaultIo: Io = {
  out: (s) => console.log(s),
  err: (s) => console.error(s),
};
