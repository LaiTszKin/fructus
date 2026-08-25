//! Pure funding-engine mirror of `programs/fructus/src/funding.rs` (issue #6).
//!
//! Units (all `APY_SCALE = 1_000_000` fixed point):
//! * `mark` — order-book mid (a yield level).
//! * `index` — annualized trustless yield.
//! * `premium = mark - index` (signed `bigint`; negative half the time).
//! * `funding_rate = clamp(funding_k·premium/APY_SCALE, -max_funding, +max_funding)` (signed).
//! * `funding_payment = notional·rate/APY_SCALE · epochs × side_flow` (signed).
//!
//! **Sign convention (mirrors R-F3):** a positive premium yields a positive rate,
//! and **longs pay shorts** — a long's funding flow is `-1` when `rate > 0`, a
//! short's is `+1`, and the two are exact opposites.
//!
//! `bigint` is arbitrary precision, so the signed math is exact in JS; the Rust
//! `saturating_*`/`checked_*` overflow guards are free here. Division truncates
//! toward zero (`BigInt` semantics), matching Rust integer division.

import { APY_SCALE } from "./constants.js";

/** Signed funding flow direction for a position side (mirrors `SideFlow`). */
export enum SideFlow {
  /** A long pays on positive funding (flow = -1). */
  Long = -1,
  /** A short receives on positive funding (flow = +1). */
  Short = 1,
}

/** `mark - index`, signed, in `APY_SCALE` fixed point (R-F1). */
export function premium(mark: bigint, index: bigint): bigint {
  return mark - index;
}

/**
 * The per-epoch funding rate, clamped to `[-max_funding, +max_funding]` (R-F2).
 *
 * `funding_k·premium` is scaled back to `APY_SCALE` by a single signed division
 * (truncating toward zero, so the sign is preserved), then clamped symmetric
 * about 0.
 */
export function fundingRate(premiumValue: bigint, fundingK: bigint, maxFunding: bigint): bigint {
  const raw = fundingK * premiumValue;
  const unscaled = raw / APY_SCALE;
  const cap = maxFunding;
  return max(-cap, min(cap, unscaled));
}

/**
 * The signed funding payment a position accrues over `epochs` full epochs
 * (R-F3, R-F5): `notional·rate/APY_SCALE · epochs × side_flow`.
 *
 * * `epochs == 0` ⇒ `0` (idempotent accrual).
 * * `rate > 0` ⇒ long flow negative (pays), short flow positive (receives).
 * * `rate < 0` ⇒ the sign flips (shorts pay).
 *
 * Truncation toward zero means the payment is `0` whenever
 * `|notional·rate| < APY_SCALE` for a single epoch (the quantization floor).
 */
export function fundingPayment(
  notional: bigint,
  rate: bigint,
  epochs: bigint,
  side: SideFlow,
): bigint {
  const scaled = (notional * rate) / APY_SCALE;
  return scaled * epochs * BigInt(side);
}

/**
 * The funding epoch index containing `slot`: `slot / funding_epoch_slots`
 * (R-F5). A degenerate zero epoch length collapses every slot to epoch 0.
 */
export function fundingEpoch(slot: bigint, epochSlots: bigint): bigint {
  if (epochSlots === 0n) {
    return 0n;
  }
  return slot / epochSlots;
}

/** Convert an on-chain `position.side` byte (`0` = Long, `1` = Short) to `SideFlow`. */
export function sideFlowFromSideByte(side: number): SideFlow {
  return side === 0 ? SideFlow.Long : SideFlow.Short;
}

function min(a: bigint, b: bigint): bigint {
  return a < b ? a : b;
}

function max(a: bigint, b: bigint): bigint {
  return a > b ? a : b;
}
