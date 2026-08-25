import { test } from "node:test";
import assert from "node:assert/strict";
import { APY_SCALE } from "../src/constants.js";
import {
  fundingEpoch,
  fundingPayment,
  fundingRate,
  premium,
  SideFlow,
  sideFlowFromSideByte,
} from "../src/funding.js";

// Cross-language lock (R-SDK3a): these vectors freeze the on-chain
// `programs/fructus/src/funding.rs` formulas over fixed inputs. They are
// asserted in TS by the trader SDK, mirroring the Rust `proptest` invariants.

test("premium is the signed market minus index difference", () => {
  assert.equal(premium(1_200_000n, 1_000_000n), 200_000n);
  assert.equal(premium(1_000_000n, 1_200_000n), -200_000n);
  assert.equal(premium(1_000_000n, 1_000_000n), 0n);
  // Mark above index => positive premium (longs pay); below => negative.
  assert.ok(premium(2_000_000n, 1_000_000n) > 0n);
  assert.ok(premium(1_000_000n, 2_000_000n) < 0n);
});

test("fundingRate clamps to +-maxFunding and is odd in premium", () => {
  // (100_000 x 200_000) / 1_000_000 = 20_000, clamped to +-10_000.
  assert.equal(fundingRate(200_000n, 100_000n, 10_000n), 10_000n);
  assert.equal(fundingRate(-200_000n, 100_000n, 10_000n), -10_000n);
  // Not clamped.
  assert.equal(fundingRate(20_000n, 100_000n, 10_000n), 2_000n);
  assert.equal(fundingRate(-20_000n, 100_000n, 10_000n), -2_000n);
  // Clamps to the cap, not beyond.
  assert.equal(fundingRate(100_000_000n, 500_000n, 1_000_000n), 1_000_000n);
  // Zero premium or zero funding_k => zero.
  assert.equal(fundingRate(0n, 100_000n, 10_000n), 0n);
  assert.equal(fundingRate(50_000n, 0n, 10_000n), 0n);
});

test("fundingPayment follows the R-F3 sign convention (long pays, short receives)", () => {
  // rate > 0, epochs = 2, notional = 1e9.
  const long = fundingPayment(1_000_000_000n, 5_000n, 2n, SideFlow.Long);
  const short = fundingPayment(1_000_000_000n, 5_000n, 2n, SideFlow.Short);
  assert.equal(long, -10_000_000n);
  assert.equal(short, 10_000_000n);
  assert.equal(long, -short, "long/short are exact opposites");

  // rate < 0 flips the signs.
  const longNeg = fundingPayment(1_000_000_000n, -5_000n, 2n, SideFlow.Long);
  const shortNeg = fundingPayment(1_000_000_000n, -5_000n, 2n, SideFlow.Short);
  assert.equal(longNeg, 10_000_000n);
  assert.equal(shortNeg, -10_000_000n);
});

test("fundingPayment is zero for zero epochs or a quantized-out rate", () => {
  assert.equal(fundingPayment(1_000_000n, 5_000n, 0n, SideFlow.Long), 0n);
  assert.equal(fundingPayment(0n, 5_000n, 2n, SideFlow.Long), 0n);
  // |notional x rate| < APY_SCALE => the single-epoch payment truncates to 0.
  assert.equal(fundingPayment(10n, 1_000n, 1n, SideFlow.Long), 0n);
});

test("fundingPayment scales linearly with epochs", () => {
  const p1 = fundingPayment(1_000_000n, 5_000n, 1n, SideFlow.Short);
  const p2 = fundingPayment(1_000_000n, 5_000n, 2n, SideFlow.Short);
  assert.equal(p1 * 2n, p2);
});

test("fundingEpoch derives the slot window", () => {
  assert.equal(fundingEpoch(0n, 10n), 0n);
  assert.equal(fundingEpoch(9n, 10n), 0n);
  assert.equal(fundingEpoch(10n, 10n), 1n);
  assert.equal(fundingEpoch(19n, 10n), 1n);
  assert.equal(fundingEpoch(20n, 10n), 2n);
  // A zero epoch length collapses to epoch 0.
  assert.equal(fundingEpoch(123n, 0n), 0n);
});

test("sideFlowFromSideByte maps 0/1 to Long/Short", () => {
  assert.equal(sideFlowFromSideByte(0), SideFlow.Long);
  assert.equal(sideFlowFromSideByte(1), SideFlow.Short);
  assert.equal(SideFlow.Long, -1);
  assert.equal(SideFlow.Short, 1);
});

test("APY_SCALE constant matches the program", () => {
  assert.equal(APY_SCALE, 1_000_000n);
});
