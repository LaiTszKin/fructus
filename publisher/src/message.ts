import { createHash } from "node:crypto";
import { PublicKey } from "@solana/web3.js";

/**
 * Canonical message construction — MUST match the on-chain `update_message` in
 * `programs/fructus/src/state.rs`:
 *
 *   sha256("fructus::update_apy" ‖ oracle_address ‖ apy_le(8) ‖ version_le(8))
 */
export const UPDATE_DOMAIN_SEPARATOR = Buffer.from("fructus::update_apy", "utf8");

/** Anchor 8-byte instruction discriminator for `update_apy`. */
export function updateApyDiscriminator(): Buffer {
  return createHash("sha256").update("global:update_apy").digest().subarray(0, 8);
}

/** The canonical 32-byte message the publisher signs. */
export function updateMessage(oracle: PublicKey, apy: bigint, version: bigint): Buffer {
  const buf = Buffer.concat([
    UPDATE_DOMAIN_SEPARATOR,
    oracle.toBuffer(),
    writeU64LE(apy),
    writeU64LE(version),
  ]);
  return createHash("sha256").update(buf).digest();
}

export function writeU64LE(n: bigint): Buffer {
  const buf = Buffer.alloc(8);
  buf.writeBigUInt64LE(n);
  return buf;
}
