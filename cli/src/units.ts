//! Small parsing helpers: sides, integers, and fixed-point scaling for
//! human-friendly input (R-C1).

import { PositionSide } from "fructus-sdk/src/index.js";
import { die } from "./errors.js";

/**
 * A human-readable side token maps to the on-chain `position.side` byte / the
 * `PositionSide` enum (`0` = Long/Bid, `1` = Short/Ask).
 */
export function parseSide(raw: string, label = "side"): number {
  const s = raw.trim().toLowerCase();
  switch (s) {
    case "long":
    case "bid":
    case "buy":
    case "l":
    case "0":
      return PositionSide.Long;
    case "short":
    case "ask":
    case "sell":
    case "s":
    case "1":
      return PositionSide.Short;
    default:
      return die(`invalid ${label}: "${raw}" (expected long|short|bid|ask)`);
  }
}

/**
 * Parse a non-negative integer (scaled by `scaleFactor` when a decimal is
 * given) into raw base units. Negative input, or a non-numeric string, throws.
 *
 * * `"1000"`, `scaleFactor = 1n` → `1000n`
 * * `"1.5"`, `scaleFactor = 1_000_000n` → `1_500_000n`
 */
export function parseUsize(
  raw: string,
  label: string,
  scaleFactor = 1n,
): bigint {
  const s = raw.trim();
  if (s.length === 0) {
    return die(`missing ${label}`);
  }
  if (s.startsWith("-")) {
    return die(`invalid ${label}: "${raw}" (must be non-negative)`);
  }
  if (s.includes(".")) {
    const [w, f = ""] = s.split(".");
    if (!/^\d*$/.test(w) || !/^\d*$/.test(f)) {
      return die(`invalid ${label}: "${raw}"`);
    }
    const fracLen = BigInt(f.length);
    const wholeScaled = BigInt(w === "" ? "0" : w) * scaleFactor;
    const fracScaled = (BigInt(f === "" ? "0" : f) * scaleFactor) / 10n ** fracLen;
    return wholeScaled + fracScaled;
  }
  if (!/^\d+$/.test(s)) {
    return die(`invalid ${label}: "${raw}" (expected a non-negative integer)`);
  }
  return BigInt(s) * scaleFactor;
}

/** Parse a signed integer string to a `bigint`. */
export function parseInt(raw: string, label: string): bigint {
  const s = raw.trim();
  if (!/^-?\d+$/.test(s)) {
    return die(`invalid ${label}: "${raw}" (expected an integer)`);
  }
  return BigInt(s);
}

/** Format a raw base-unit `bigint` for display. */
export function fmt(value: bigint): string {
  return value.toString();
}
