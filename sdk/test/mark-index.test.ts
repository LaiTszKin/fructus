import { test } from "node:test";
import assert from "node:assert/strict";
import { PublicKey } from "@solana/web3.js";
import { SLOTS_PER_YEAR } from "../src/constants.js";
import { annualize, realizedYield } from "../src/exchange.js";
import { mid, twap } from "../src/orderbook.js";
import { expectedFundingPayment, expectedIndex, midFromBook, twapFromBook } from "../src/mark-index.js";

// Cross-language lock (R-SDK3): the mark/index + expected-funding helpers mirror
// the on-chain `settle_funding` adapter (index = annualized realized yield from
// the market baseline; mark = order-book mid; rate = clamped premium; payment =
// notional·rate·epochs·side_flow).

test("realizedYield + annualize compute the trustless index", () => {
  // baseline rate 1.0 -> current rate 2.0.
  const open = { totalLamports: 100_000_000n, poolTokenSupply: 100_000_000n };
  const settle = { totalLamports: 100_000_000n, poolTokenSupply: 50_000_000n };
  const r = realizedYield(open, settle)!;
  assert.equal(r, 1_000_000n);
  const idx = annualize(r, SLOTS_PER_YEAR, SLOTS_PER_YEAR)!;
  assert.equal(idx, 1_000_000n);
});

test("expectedIndex mirrors the on-chain index (baseline vs live pool)", () => {
  const baseline = { totalLamports: 100_000_000n, poolTokenSupply: 100_000_000n };
  const current = { totalLamports: 100_000_000n, poolTokenSupply: 50_000_000n };
  assert.equal(expectedIndex(baseline, current, SLOTS_PER_YEAR), 1_000_000n);
  // Same current rate => realized yield 0 => index 0.
  const flat = { totalLamports: 100_000_000n, poolTokenSupply: 100_000_000n };
  assert.equal(expectedIndex(baseline, flat, 1_000n), 0n);
  // No baseline (first settlement) => 0.
  assert.equal(expectedIndex(null, current, 1_000n), 0n);
});

test("expectedFundingPayment mirrors the settle_funding pipeline", () => {
  const market = {
    indexSource: PublicKey.unique(),
    collateralMint: PublicKey.unique(),
    fundingK: 100_000n,
    maxFunding: 10_000n,
    fundingEpochSlots: 1_000n,
    initialMarginBps: 1_000,
    maintenanceMarginBps: 500,
    authority: PublicKey.unique(),
    vault: PublicKey.unique(),
    fundingEpoch: 1n,
    indexN: 100_000_000n,
    indexD: 100_000_000n,
    fundingAccumulator: 0n,
    bump: 255,
  };
  const position = {
    market: market.indexSource, // placeholder
    owner: PublicKey.unique(),
    side: 0, // Long
    notional: 10_000_000n,
    entryN: 0n,
    entryD: 0n,
    collateral: 1_000_000n,
    lastFundingEpoch: 0n,
    closedNotional: 0n,
    openSlot: 0n,
    bump: 1,
  };
  const nowSlot = 1_000n; // epoch 1
  const mark = 1_050_000n;
  const current = { totalLamports: 100_000_000n, poolTokenSupply: 100_000_000n };

  const payment = expectedFundingPayment(market, position, mark, current, nowSlot)!;
  // premium = 1_050_000 - 0 = 1_050_000; rate clamps to 10_000;
  // payment = 10_000_000 * 10_000 / 1_000_000 * 1 * (-1) = -100_000 (long pays).
  assert.equal(payment, -100_000n);
});

test("mid / midFromBook compute the order-book mark", () => {
  assert.equal(mid(1_050_000n, 1_100_000n), (1_050_000n + 1_100_000n) / 2n);
  assert.equal(mid(0n, 1_100_000n), null);
  const book = {
    nextSeq: 0n,
    bestBid: 1_050_000n,
    bestAsk: 1_100_000n,
    eventReadCursor: 0n,
    eventWriteCursor: 0n,
    twapCursor: 0n,
    market: PublicKey.unique(),
    bump: 0,
    bids: [],
    asks: [],
    events: [],
    observations: [],
  };
  assert.equal(midFromBook(book), 1_075_000n);
});

test("twap / twapFromBook compute the liquidation reference price", () => {
  const obs = [
    { slot: 0n, cumulativeMid: 0n },
    { slot: 100n, cumulativeMid: 100_000_000n },
  ];
  // windowSlots = 100 => avg = delta/100.
  assert.equal(twap(obs, 100n, 100n), 1_000_000n);
  // History does not reach back a full window.
  assert.equal(twap(obs, 200n, 100n), null);
  assert.equal(twap([], 100n, 100n), null);
});

test("twapFromBook uses decoded observations", () => {
  const book = {
    nextSeq: 0n,
    bestBid: 0n,
    bestAsk: 0n,
    eventReadCursor: 0n,
    eventWriteCursor: 0n,
    twapCursor: 0n,
    market: PublicKey.unique(),
    bump: 0,
    bids: [],
    asks: [],
    events: [],
    observations: [
      { slot: 0n, mid: 0n, cumulativeMid: 0n },
      { slot: 100n, mid: 1_000_000n, cumulativeMid: 100_000_000n },
    ],
  };
  assert.equal(twapFromBook(book, 100n, 100n), 1_000_000n);
});
