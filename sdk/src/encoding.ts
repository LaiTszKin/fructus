//! Anchor instruction/account discriminators and borsh-serialization helpers.
//!
//! Anchor prefixes every instruction with the first 8 bytes of
//! `sha256("global:<ix_name>")` and every `#[account]` (and `#[account(zero_copy)]`)
//! with the first 8 bytes of `sha256("account:<TypeName>")`. The `write*` helpers
//! encode instruction args in borsh's little-endian fixed-width form, matching
//! the on-chain `Context<...>` argument decoding.

import { createHash } from "node:crypto";
import { PublicKey } from "@solana/web3.js";

/** The first 8 bytes of `sha256("global:<name>")` — the Anchor ix discriminator. */
export function anchorIxDiscriminator(name: string): Buffer {
  return createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
}

/** The first 8 bytes of `sha256("account:<name>")` — the Anchor account discriminator. */
export function anchorAccountDiscriminator(name: string): Buffer {
  return createHash("sha256").update(`account:${name}`).digest().subarray(0, 8);
}

/** Encode a `u64` / `i64` as 8 little-endian bytes (borsh). */
export function writeU64LE(n: bigint): Buffer {
  const buf = Buffer.alloc(8);
  buf.writeBigUInt64LE(n);
  return buf;
}

/** Encode a signed `i64` as 8 little-endian bytes (borsh). `n` must fit in i64. */
export function writeI64LE(n: bigint): Buffer {
  const buf = Buffer.alloc(8);
  buf.writeBigInt64LE(n);
  return buf;
}

/** Encode a `u16` / `i16` as 2 little-endian bytes (borsh). */
export function writeU16LE(n: number): Buffer {
  const buf = Buffer.alloc(2);
  buf.writeUInt16LE(n);
  return buf;
}

/** Encode a `u8` as a single byte (borsh). */
export function writeU8(n: number): Buffer {
  return Buffer.from([n & 0xff]);
}

/** Encode a `Pubkey` as its 32 raw bytes. */
export function writePubkey(pk: PublicKey): Buffer {
  return pk.toBuffer();
}

/**
 * Read an unsigned 128-bit integer stored as 16 bytes little-endian (borsh
 * `u128`, or the raw `[u8; 16]` `cumulative_mid` of a zero-copy observation).
 */
export function readU128LE(buf: Buffer, offset: number): bigint {
  const lo = buf.readBigUInt64LE(offset);
  const hi = buf.readBigUInt64LE(offset + 8);
  return (hi << 64n) | lo;
}

/**
 * Read a signed 128-bit integer stored as 16 bytes little-endian (borsh `i128`),
 * e.g. `PerpMarket.funding_accumulator`.
 */
export function readI128LE(buf: Buffer, offset: number): bigint {
  const lo = buf.readBigUInt64LE(offset);
  const hi = buf.readBigUInt64LE(offset + 8);
  let value = (hi << 64n) | lo;
  // Two's-complement sign extension from bit 127.
  if (value >= 1n << 127n) {
    value -= 1n << 128n;
  }
  return value;
}

/** Read a `u64` from `data` at `ADDRESS_OFFSET`, tolerating short buffers. */
export function readU64LE(data: Buffer, offset: number): bigint {
  return data.readBigUInt64LE(offset);
}

export function readU8(data: Buffer, offset: number): number {
  return data[offset];
}
