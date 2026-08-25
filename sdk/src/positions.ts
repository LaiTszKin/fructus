//! Pure position-lifecycle mirror of `programs/fructus/src/positions.rs`.
//!
//! The on-chain module computes **signed** PnL from notional-weighted entry
//! running sums (`entry_n_sum` / `entry_d_sum`), normalized by a shared
//! power-of-two shift before the ratio is cross-multiplied. `bigint` makes the
//! arithmetic exact in JS (no overflow in the production domain), and division
//! truncates toward zero just like Rust integer division.

import { APY_SCALE } from "./constants.js";

/** Which side of the market a position holds (`0` = Long/Bid, `1` = Short/Ask). */
export enum PositionSide {
  Long = 0,
  Short = 1,
}

/** Map an on-chain `position.side` byte to `PositionSide`, or `null` for invalid. */
export function positionSideFromSideByte(side: number): PositionSide | null {
  if (side === 0) return PositionSide.Long;
  if (side === 1) return PositionSide.Short;
  return null;
}

/**
 * Reserved collateral for `notional` under `initial_margin_bps`:
 * `ceil(notional × bps / 10_000)` — CEILING division via `+ 9999` before the
 * `/ 10_000` floor, so collateral ≥ 1 for notional ≥ 1 and implied leverage
 * stays at or below the `initial_margin_bps` cap.
 */
export function marginRequired(notional: bigint, initialMarginBps: number): bigint {
  const exact = notional * BigInt(initialMarginBps);
  return (exact + 9_999n) / 10_000n;
}

/**
 * Accumulate the entry running sums: `cur += add_n × add_w`,
 * `d_cur += add_d × add_w`.
 */
export function accumulateEntry(
  curN: bigint,
  curD: bigint,
  addN: bigint,
  addD: bigint,
  addW: bigint,
): { n: bigint; d: bigint } {
  return {
    n: curN + addN * addW,
    d: curD + addD * addW,
  };
}

/**
 * Normalize the entry running sums into a `u64` rate pair by a SHARED
 * power-of-two shift: `k = max(0, bitlen(max(n_sum, d_sum)) - 45)`. The LARGER
 * sum lands in `[2^44, 2^45)`; the smaller may fall below `2^44` (down to 0).
 * Exact (`k = 0`) when both sums are `< 2^45`.
 */
export function normalizeSums(entryN: bigint, entryD: bigint): [bigint, bigint] {
  const maxVal = entryN > entryD ? entryN : entryD;
  const bitlen = maxVal === 0n ? 0 : maxVal.toString(2).length;
  const shift = (bitlen - 45) > 0 ? bitlen - 45 : 0;
  return [entryN >> BigInt(shift), entryD >> BigInt(shift)];
}

/**
 * `(rate_current / rate_entry - 1) × APY_SCALE`, WITH sign, computed as
 * `((cur_n × d_e) - (n_e × cur_d)) × APY_SCALE / (n_e × cur_d)` where
 * `(n_e, d_e) = normalize_sums(entry_n_sum, entry_d_sum)`. Sign comes from the
 * numerator. `null` on degenerate inputs (zero entry numerator/denominator or a
 * zero current component).
 */
export function signedYieldChange(
  entryN: bigint,
  entryD: bigint,
  curN: bigint,
  curD: bigint,
): bigint | null {
  const [n_e, d_e] = normalizeSums(entryN, entryD);
  if (n_e === 0n || d_e === 0n || curN === 0n || curD === 0n) {
    return null;
  }
  const curX = curN * d_e;
  const entryX = n_e * curD;
  const num = curX - entryX;
  const scaled = num * APY_SCALE;
  return scaled / entryX; // truncates toward zero, so the sign comes from `num`
}

/**
 * PnL in signed USDC microunits:
 * `notional × signed_yield_change / APY_SCALE × (+1 Long, -1 Short)`,
 * truncating toward zero (so `pnl == 0` whenever `|notional·change| < APY_SCALE`).
 */
export function pnl(
  entryN: bigint,
  entryD: bigint,
  curN: bigint,
  curD: bigint,
  notional: bigint,
  side: PositionSide,
): bigint | null {
  const change = signedYieldChange(entryN, entryD, curN, curD);
  if (change === null) return null;
  const scaled = (notional * change) / APY_SCALE;
  return side === PositionSide.Long ? scaled : -scaled;
}

/**
 * Apply signed PnL to the deposited collateral (R-S2/R-S3).
 *
 * * `pnl == 0` ⇒ `deposited` (unchanged).
 * * `pnl > 0` ⇒ `deposited + pnl` (credits the vault ledger).
 * * `pnl < 0` ⇒ `deposited - |pnl|`, **clamped at 0** so the vault is never
 *   left insolvent by a settlement.
 *
 * Unlike the Rust `Option` (which is `None` only on a positive `u64` overflow),
 * `bigint` never overflows, so this is total and returns a `bigint`.
 */
export function applyPnl(deposited: bigint, pnlValue: bigint): bigint {
  if (pnlValue >= 0n) {
    return deposited + pnlValue;
  }
  const debit = -pnlValue;
  return debit >= deposited ? 0n : deposited - debit;
}
