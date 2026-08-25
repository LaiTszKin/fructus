import { test } from "node:test";
import assert from "node:assert/strict";
import { PublicKey } from "@solana/web3.js";
import { expectedFundingPayment, type PerpMarketState, type PositionState } from "fructus-sdk/src/index.js";
import { expectedFundingForBook } from "../src/commands/funding.js";

/**
 * R-1 regression: the `settle_funding` mark fallback.
 *
 * On-chain `settle_funding` uses `mark = orderbook::mid(&book).unwrap_or(index)`.
 * The CLI must mirror that: for a one-sided/empty order book (`midFromBook`
 * returns `null`), the settle mark falls back to the **index**, so
 * `premium == 0` and `expected_funding_payment == 0`.
 *
 * The bug (fixed) was passing `mark ?? 0n`, which used a mark of `0` instead of
 * `index` — producing a spurious non-zero funding payment for an empty book.
 */

const ZERO = new PublicKey(new Uint8Array(32));

/** Baseline exchange rate = 1 (indexN == indexD). */
function market(): PerpMarketState {
  return {
    indexSource: ZERO,
    collateralMint: ZERO,
    fundingK: 100_000n,
    maxFunding: 10_000n,
    fundingEpochSlots: 1_000n,
    initialMarginBps: 1_000,
    maintenanceMarginBps: 500,
    authority: ZERO,
    vault: ZERO,
    fundingEpoch: 0n,
    indexN: 100_000_000_000_000n,
    indexD: 100_000_000_000_000n,
    fundingAccumulator: 0n,
    bump: 0,
  };
}

function longPosition(): PositionState {
  return {
    market: ZERO,
    owner: ZERO,
    side: 0, // Long
    notional: 5_000_000_000n,
    entryN: 0n,
    entryD: 0n,
    collateral: 0n,
    lastFundingEpoch: 0n,
    closedNotional: 0n,
    openSlot: 0n,
    bump: 0,
  };
}

/** Pool rate now = 2 (totalLamports / poolTokenSupply = 2e14 / 1e14). */
const CURRENT = { totalLamports: 200_000_000_000_000n, poolTokenSupply: 100_000_000_000_000n };
const NOW_SLOT = 10_000n; // 10 full epochs at fundingEpochSlots = 1000

test("R-1: empty book (mark=null) falls back to index => expected funding payment is 0", () => {
  const m = market();
  const pos = longPosition();
  assert.equal(expectedFundingForBook(m, pos, null, CURRENT, NOW_SLOT), 0n);
});

test("R-1: mark == index also yields 0 (premium 0), mirroring the Rust unwrap_or(index)", () => {
  const m = market();
  const pos = longPosition();
  const index = 7_884_000_000n; // annualize(realized yield 1e6, 10000 slots, SLOTS_PER_YEAR)
  assert.equal(expectedFundingForBook(m, pos, index, CURRENT, NOW_SLOT), 0n);
});

test("R-1: the old buggy path (mark = 0) produced a spurious non-zero payment", () => {
  const m = market();
  const pos = longPosition();
  // The SDK with a hardcoded mark 0 (what the CLI used to pass) yields a nonzero
  // payment; this documents the failure mode the fallback fix avoids.
  const buggy = expectedFundingPayment(m, pos, 0n, CURRENT, NOW_SLOT);
  assert.notEqual(buggy, 0n);
  assert.equal(buggy, 500_000_000n);
});

test("R-1: a one-sided book (mark=null) on the short side also yields 0", () => {
  const m = market();
  const pos: PositionState = { ...longPosition(), side: 1 }; // Short
  assert.equal(expectedFundingForBook(m, pos, null, CURRENT, NOW_SLOT), 0n);
});
