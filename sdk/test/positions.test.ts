import { test } from "node:test";
import assert from "node:assert/strict";
import { APY_SCALE } from "../src/constants.js";
import {
  accumulateEntry,
  applyPnl,
  marginRequired,
  normalizeSums,
  pnl,
  positionSideFromSideByte,
  PositionSide,
  signedYieldChange,
} from "../src/positions.js";

// Cross-language lock (R-SDK3): mirrors `programs/fructus/src/positions.rs`.

test("marginRequired uses ceiling division", () => {
  assert.equal(marginRequired(1_000_000n, 1000), 100_000n);
  // bps == 10_000 => exact 1:1.
  assert.equal(marginRequired(1_000_000n, 10_000), 1_000_000n);
  assert.equal(marginRequired(0n, 1), 0n);
  // Ceiling: 1 notional at bps=1000 => ceil(0.1) = 1.
  assert.equal(marginRequired(1n, 1000), 1n);
  // 10 notional at bps=1000 => ceil(1) = 1 (exact, no bump).
  assert.equal(marginRequired(10n, 1000), 1n);
  // 11 notional at bps=1000 => ceil(1.1) = 2.
  assert.equal(marginRequired(11n, 1000), 2n);
});

test("accumulateEntry accumulates weighted running sums", () => {
  const s = accumulateEntry(0n, 0n, 100_000_000n, 100_000_000n, 1_000n);
  assert.equal(s.n, 100_000_000n * 1_000n);
  assert.equal(s.d, 100_000_000n * 1_000n);
  // Accumulate a second component.
  const s2 = accumulateEntry(s.n, s.d, 100_000_000n, 100_000_000n, 3_000n);
  assert.equal(s2.n, 100_000_000n * 4_000n);
  assert.equal(s2.d, 100_000_000n * 4_000n);
});

test("normalizeSums is exact below 2^45 and windowed above", () => {
  assert.deepEqual(normalizeSums(0n, 0n), [0n, 0n]);
  assert.deepEqual(normalizeSums(APY_SCALE, APY_SCALE), [APY_SCALE, APY_SCALE]);
  // Large sums: the larger component lands in [2^44, 2^45).
  for (const shift of [45, 46, 60, 90]) {
    const big = 1n << BigInt(shift);
    const [an, ad] = normalizeSums(big, big);
    assert.ok(an >= (1n << 44n) && an < (1n << 45n));
    assert.ok(ad >= (1n << 44n) && ad < (1n << 45n));
  }
});

test("signedYieldChange and pnl mirror the entry-running-sum model", () => {
  // Entry rate = 1.0; current rate doubles (d halves) => +100% yield change.
  const entry = accumulateEntry(0n, 0n, 100_000_000n, 100_000_000n, 1_000n);
  const change = signedYieldChange(entry.n, entry.d, 100_000_000n, 50_000_000n);
  assert.equal(change, 1_000_000n);

  const pLong = pnl(entry.n, entry.d, 100_000_000n, 50_000_000n, 10_000_000n, PositionSide.Long);
  const pShort = pnl(entry.n, entry.d, 100_000_000n, 50_000_000n, 10_000_000n, PositionSide.Short);
  assert.equal(pLong, 10_000_000n);
  assert.equal(pShort, -10_000_000n);
  assert.equal(pLong, -pShort);

  // Equal entry/current => 0 pnl.
  assert.equal(pnl(entry.n, entry.d, 100_000_000n, 100_000_000n, 10_000_000n, PositionSide.Long), 0n);
  // Degenerate entry sums => null.
  assert.equal(signedYieldChange(0n, 0n, 100_000_000n, 50_000_000n), null);
  assert.equal(pnl(0n, 0n, 100_000_000n, 50_000_000n, 10_000_000n, PositionSide.Long), null);
});

test("pnl long profits when the index rises and loses when it falls", () => {
  const entry = accumulateEntry(0n, 0n, 100_000_000n, 100_000_000n, 1_000n);
  // Current rate higher (d halves) => long profits.
  assert.ok(pnl(entry.n, entry.d, 100_000_000n, 50_000_000n, 5_000_000n, PositionSide.Long)! > 0n);
  // Current rate lower (n halves) => long loses.
  assert.ok(pnl(entry.n, entry.d, 50_000_000n, 100_000_000n, 5_000_000n, PositionSide.Long)! < 0n);
});

test("applyPnl credits profits, debits losses clamped at zero", () => {
  assert.equal(applyPnl(1_000_000n, 250_000n), 1_250_000n);
  assert.equal(applyPnl(1_000_000n, -250_000n), 750_000n);
  // Loss clamps so the vault is never negative (R-S3).
  assert.equal(applyPnl(500_000n, -1_000_000n), 0n);
  assert.equal(applyPnl(1_000_000n, 0n), 1_000_000n);
  assert.equal(applyPnl(0n, -1n), 0n);
});

test("positionSideFromSideByte maps the on-chain encoding", () => {
  assert.equal(positionSideFromSideByte(0), PositionSide.Long);
  assert.equal(positionSideFromSideByte(1), PositionSide.Short);
  assert.equal(positionSideFromSideByte(255), null);
});
