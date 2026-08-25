import { test } from "node:test";
import assert from "node:assert/strict";

// Independent adversarial invariant tests (issues #6–#11) for the TS SDK
// mirrors. These replicate the *Rust* reference formulas (read directly from
// programs/fructus/src/{funding,positions,liquidation,exchange,orderbook}.rs)
// using exact BigInt math, and assert the SDK mirrors are byte-identical. Any
// divergence (sign, clamp, annualize, twap, quantization) is a confirmed defect.

import { APY_SCALE, SLOTS_PER_YEAR } from "../src/constants.js";
import {
  fundingEpoch,
  fundingPayment,
  fundingRate,
  premium,
  SideFlow,
} from "../src/funding.js";
import {
  accumulateEntry,
  applyPnl,
  marginRequired,
  normalizeSums,
  pnl,
  PositionSide,
  signedYieldChange,
} from "../src/positions.js";
import { mid, twap } from "../src/orderbook.js";
import { annualize, realizedYield } from "../src/exchange.js";
import { expectedFundingPayment, expectedIndex } from "../src/mark-index.js";

// A deterministic xorshift64 PRNG so a divergence reproduces exactly.
function xorshift(seed: number): () => number {
  let s = BigInt(seed >>> 0) || 1n;
  return () => {
    s ^= s << 13n;
    s ^= s >> 7n;
    s ^= s << 17n;
    s &= 0xffffffffffffffffn;
    return Number(s % 1000000000000000000n);
  };
}

function bigInRange(rng: () => number, lo: bigint, hi: bigint): bigint {
  const span = hi - lo;
  return lo + (BigInt(rng()) % (span + 1n));
}

// ---------------------------------------------------------------------------
// funding.ts — R-F1/R-F2/R-F3/R-F5
// ---------------------------------------------------------------------------

test("premium is the exact signed difference across the full u64 domain", () => {
  const rng = xorshift(0x12345678);
  for (let i = 0; i < 10_000; i++) {
    const mark = bigInRange(rng, 0n, 0xffffffffffffffffn);
    const index = bigInRange(rng, 0n, 0xffffffffffffffffn);
    const p = premium(mark, index);
    assert.equal(p, mark - index, "premium must equal mark-index exactly");
    assert.equal(premium(index, mark), -p, "antisymmetric");
    assert.equal(mark > index, p > 0n, "mark>index => positive premium");
    assert.equal(mark < index, p < 0n, "mark<index => negative premium");
    assert.equal(mark === index, p === 0n, "premium==0 iff equal");
  }
});

test("fundingRate clamps to +-maxFunding and matches the Rust formula", () => {
  const rng = xorshift(0xbeef);
  for (let i = 0; i < 10_000; i++) {
    const p = bigInRange(rng, -APY_SCALE * 1000n, APY_SCALE * 1000n);
    const k = bigInRange(rng, 1n, APY_SCALE);
    const cap = bigInRange(rng, 0n, APY_SCALE);
    // Rust reference: raw = k*premium; unscaled = raw/APY_SCALE (trunc->0);
    // clamp to [-cap, cap].
    let unscaled = (k * p) / APY_SCALE;
    if (unscaled > cap) unscaled = cap;
    else if (unscaled < -cap) unscaled = -cap;
    assert.equal(fundingRate(p, k, cap), unscaled, "clamped rate");
    // odd in premium
    assert.equal(fundingRate(p, k, cap), -fundingRate(-p, k, cap), "odd");
    // monotonic for p2>=p1
    const p2 = p + 1n;
    assert.ok(fundingRate(p2, k, cap) >= unscaled, "non-decreasing");
  }
});

test("fundingPayment sign convention and exact opposites across a wide band", () => {
  const rng = xorshift(0xdead);
  for (let i = 0; i < 10_000; i++) {
    const notional = bigInRange(rng, 1n, 1_000_000_000_000_000n);
    const rate = bigInRange(rng, -APY_SCALE * 10n, APY_SCALE * 10n);
    const epochs = bigInRange(rng, 1n, 10_000n);
    const pLong = fundingPayment(notional, rate, epochs, SideFlow.Long);
    const pShort = fundingPayment(notional, rate, epochs, SideFlow.Short);
    assert.equal(pLong, -pShort, "long and short are exact opposites");
    const scaled = (notional * rate) / APY_SCALE;
    assert.equal(pLong, -scaled * epochs, "matches notional*rate/APY_SCALE*epochs*flow");
    if (rate > 0n) {
      assert.ok(pLong <= 0n, "positive rate => long never gains");
      assert.ok(pShort >= 0n, "positive rate => short never loses");
      if (scaled > 0n) {
        assert.ok(pLong < 0n, "positive funding => long pays");
        assert.ok(pShort > 0n, "positive funding => short receives");
      }
    } else if (rate < 0n) {
      assert.ok(pLong >= 0n, "negative rate => long never loses");
      assert.ok(pShort <= 0n, "negative rate => short never gains");
      if (scaled < 0n) {
        assert.ok(pLong > 0n, "negative funding => long receives");
        assert.ok(pShort < 0n, "negative funding => short pays");
      }
    }
  }
});

test("fundingEpoch is floor division and collapses a zero epoch length", () => {
  const rng = xorshift(0xcafe);
  for (let i = 0; i < 10_000; i++) {
    const slot = bigInRange(rng, 0n, 0xffffffffffffffffn);
    const epochSlots = bigInRange(rng, 1n, 1_000_000n);
    assert.equal(fundingEpoch(slot, epochSlots), slot / epochSlots);
    assert.ok(fundingEpoch(slot + 1n, epochSlots) >= fundingEpoch(slot, epochSlots));
  }
  assert.equal(fundingEpoch(0n, 0n), 0n);
  assert.equal(fundingEpoch(1_000_000n, 0n), 0n);
});

test("fundingPayment zero epochs / zero rate are no-ops", () => {
  const rng = xorshift(0xf00d);
  for (let i = 0; i < 10_000; i++) {
    const notional = bigInRange(rng, 0n, 1_000_000_000_000_000n);
    const rate = bigInRange(rng, -APY_SCALE * 10n, APY_SCALE * 10n);
    const epochs = bigInRange(rng, 0n, 10_000n);
    assert.equal(fundingPayment(notional, rate, 0n, SideFlow.Long), 0n);
    assert.equal(fundingPayment(notional, rate, 0n, SideFlow.Short), 0n);
    assert.equal(fundingPayment(notional, 0n, epochs, SideFlow.Long), 0n);
    assert.equal(fundingPayment(notional, 0n, epochs, SideFlow.Short), 0n);
  }
});

// ---------------------------------------------------------------------------
// positions.ts — apply_pnl / pnl / margin_required / accumulate_entry
// ---------------------------------------------------------------------------

test("applyPnl credits, clamps losses at zero, and never goes negative", () => {
  const rng = xorshift(0xabcd);
  for (let i = 0; i < 10_000; i++) {
    const deposited = bigInRange(rng, 0n, 0xffffffffffffffffn);
    const gain = bigInRange(rng, 0n, 0xffffffffffffffffn);
    assert.equal(applyPnl(deposited, gain), deposited + gain, "profit credits");
    assert.equal(applyPnl(deposited, -gain), gain >= deposited ? 0n : deposited - gain, "loss clamps");
    assert.equal(applyPnl(deposited, 0n), deposited, "zero is a no-op");
  }
});

test("pnl long/short antisymmetry and sign across a wide band", () => {
  const rng = xorshift(0x9876);
  const lo = 100_000_000_000_000n;
  const hi = 100_000_000_000_000_000n;
  for (let i = 0; i < 10_000; i++) {
    const n = bigInRange(rng, lo, hi);
    const d = bigInRange(rng, lo, hi);
    const curN = bigInRange(rng, lo, hi);
    const curD = bigInRange(rng, lo, hi);
    const w = bigInRange(rng, 1_000_000n, 1_000_000_000_000n);
    const notional = bigInRange(rng, 1n, 1_000_000_000_000_000_000n);
    const [ns, ds] = (() => { const r = accumulateEntry(0n, 0n, n, d, w); return [r.n, r.d]; })();
    const pLong = pnl(ns, ds, curN, curD, notional, PositionSide.Long);
    const pShort = pnl(ns, ds, curN, curD, notional, PositionSide.Short);
    if (pLong !== null && pShort !== null) {
      assert.equal(pLong, -pShort, "long and short pnl are exact opposites");
      const change = signedYieldChange(ns, ds, curN, curD)!;
      if (change > 0n) assert.ok(pLong >= 0n, "index up => long non-negative");
      else if (change < 0n) assert.ok(pLong <= 0n, "index down => long non-positive");
      else assert.equal(pLong, 0n, "no change => zero pnl");
    }
  }
});

test("marginRequired is ceil(notional*bps/10000) and monotonic", () => {
  const rng = xorshift(0x1357);
  for (let i = 0; i < 10_000; i++) {
    const notional = bigInRange(rng, 0n, 0xffffffffffffffffn);
    const bps = bigInRange(rng, 1n, 10_000n);
    const expected = (notional * bps + 9_999n) / 10_000n;
    assert.equal(marginRequired(notional, Number(bps)), expected, "ceiling formula");
    assert.ok(marginRequired(notional + 1n, Number(bps)) >= expected, "monotonic in notional");
    if (bps === 10_000n) assert.equal(marginRequired(notional, 10000), notional, "1x at 10_000 bps");
  }
});

test("normalizeSums lands the larger component in [2^44, 2^45) or stays exact", () => {
  const rng = xorshift(0x2468);
  for (let i = 0; i < 10_000; i++) {
    const n = bigInRange(rng, 0n, 0xffffffffffffffffffffffffffffffffn); // ~u128
    const d = bigInRange(rng, 0n, 0xffffffffffffffffffffffffffffffffn);
    const [an, ad] = normalizeSums(n, d);
    const maxVal = n > d ? n : d;
    const bitlen = maxVal === 0n ? 0 : maxVal.toString(2).length;
    if (bitlen > 45) {
      const larger = n >= d ? an : ad;
      assert.ok(larger >= 2n ** 44n && larger < 2n ** 45n, "larger in [2^44,2^45)");
    } else {
      assert.equal(an, n, "exact below 2^45");
      assert.equal(ad, d, "exact below 2^45");
    }
  }
});

// ---------------------------------------------------------------------------
// exchange.ts + orderbook.ts + mark-index.ts — annualize / mid / twap / index
// ---------------------------------------------------------------------------

test("realizedYield / annualize match Rust semantics", () => {
  const rng = xorshift(0x5a5a);
  for (let i = 0; i < 10_000; i++) {
    const open = { totalLamports: bigInRange(rng, 1n, 0xffffffffffffffffn), poolTokenSupply: bigInRange(rng, 1n, 0xffffffffffffffffn) };
    const settle = { totalLamports: bigInRange(rng, 1n, 0xffffffffffffffffn), poolTokenSupply: bigInRange(rng, 1n, 0xffffffffffffffffn) };
    const ry = realizedYield(open, settle);
    const a = settle.totalLamports * open.poolTokenSupply;
    const b = open.totalLamports * settle.poolTokenSupply;
    if (a < b) {
      assert.equal(ry, 0n, "negative yield clamps to 0");
    } else {
      assert.equal(ry, ((a - b) * APY_SCALE) / b, "realized yield formula");
    }
    const period = bigInRange(rng, 1n, 1_000_000n);
    assert.equal(annualize(ry!, period, SLOTS_PER_YEAR), (ry! * SLOTS_PER_YEAR) / period, "annualize");
    assert.equal(annualize(ry!, 0n, SLOTS_PER_YEAR), null, "zero period => null");
  }
});

test("mid returns (bid+ask)/2 or null when a side is empty", () => {
  const rng = xorshift(0x7777);
  for (let i = 0; i < 10_000; i++) {
    const bid = bigInRange(rng, 0n, 0xffffffffffffffffn);
    const ask = bigInRange(rng, 0n, 0xffffffffffffffffn);
    if (bid === 0n || ask === 0n) assert.equal(mid(bid, ask), null, "one-sided => null");
    else assert.equal(mid(bid, ask), (bid + ask) / 2n, "mid = (bid+ask)/2");
  }
});

test("twap matches the cumulative-delta / window formula", () => {
  const rng = xorshift(0x8888);
  for (let i = 0; i < 10_000; i++) {
    const window = bigInRange(rng, 1n, 10_000n);
    const now = bigInRange(rng, window + 1n, window + 1_000_000n);
    // A synthetic observation ring consistent with a constant mid `m`:
    // cumulative_mid(slot) = m * (slot - firstSlot), so it is piecewise-linear.
    const m = bigInRange(rng, 1n, 1_000_000n);
    const firstSlot = now - window - 50n;
    const obs = [
      { slot: firstSlot, cumulativeMid: 0n },
      { slot: now, cumulativeMid: m * (now - firstSlot) },
    ];
    const t = twap(obs, window, now);
    assert.equal(t, m, "constant mid => twap = that mid");
    assert.equal(twap([], window, now), null, "empty history => null");
  }
});

test("expectedIndex is 0 without a baseline and annualizes the baseline yield", () => {
  const rng = xorshift(0x9999);
  for (let i = 0; i < 10_000; i++) {
    const current = { totalLamports: bigInRange(rng, 1n, 0xffffffffffffffffn), poolTokenSupply: bigInRange(rng, 1n, 0xffffffffffffffffn) };
    const elapsed = bigInRange(rng, 1n, 1_000_000n);
    assert.equal(expectedIndex(null, current, elapsed), 0n, "no baseline => 0");
    const baseline = { totalLamports: bigInRange(rng, 1n, 0xffffffffffffffffn), poolTokenSupply: bigInRange(rng, 1n, 0xffffffffffffffffn) };
    const ry = realizedYield(baseline, current);
    if (ry !== null) {
      assert.equal(expectedIndex(baseline, current, elapsed), (ry * SLOTS_PER_YEAR) / elapsed, "baseline index annualized");
    }
  }
});

// ---------------------------------------------------------------------------
// mark-index.ts — expectedFundingPayment mirrors the Rust settle_funding adapter
// ---------------------------------------------------------------------------

test("expectedFundingPayment mirrors settle_funding (mark fallback, clamp, side flow)", () => {
  // Fixed, fully-deterministic scenario: baseline set, current rate higher,
  // book two-sided with a mark above the index.
  const market: any = {
    fundingEpochSlots: 1000n,
    fundingK: 100_000n,
    maxFunding: 10_000n,
    indexN: 100_000_000_000_000n,
    indexD: 100_000_000_000_000n,
  };
  const currentRate = { totalLamports: 200_000_000_000_000n, poolTokenSupply: 100_000_000_000_000n };
  const posLong: any = { notional: 5_000_000_000n, lastFundingEpoch: 0n, side: 0 };

  // nowSlot in epoch 10 => 10 full epochs elapsed.
  const nowSlot = 10_000n;
  const mark = 1_200_000n; // above the annualized index

  const p = expectedFundingPayment(market, posLong, mark, currentRate, nowSlot);
  assert.notEqual(p, null, "expectedFundingPayment should compute");
  // Replicate the Rust adapter by hand.
  const curEpoch = fundingEpoch(nowSlot, market.fundingEpochSlots);
  const ep = curEpoch - posLong.lastFundingEpoch;
  const elapsed = ep * market.fundingEpochSlots;
  const baseline = { totalLamports: market.indexN, poolTokenSupply: market.indexD };
  const idx = expectedIndex(baseline, currentRate, elapsed);
  const prem = premium(mark, idx!);
  const rate = fundingRate(prem, market.fundingK, market.maxFunding);
  const flow = posLong.side === 0 ? SideFlow.Long : SideFlow.Short;
  const expected = fundingPayment(posLong.notional, rate, ep, flow);
  assert.equal(p, expected, "expectedFundingPayment matches the Rust settle_funding math");

  // One-sided book => premium 0 => zero payment (mark falls back to index).
  const pEmpty = expectedFundingPayment(market, posLong, idx!, currentRate, nowSlot);
  assert.equal(pEmpty, 0n, "one-sided/empty book => premium==0 => no funding");

  // CLI divergence probe: `cli/src/commands/funding.ts` passes `mark ?? 0n`
  // when the book is empty, replacing the Rust `mid().unwrap_or(index)`
  // fallback with a literal 0. That yields a NEGATIVE premium (-index) and a
  // NON-ZERO payment, whereas the Rust `settle_funding` yields exactly 0.
  // Reproduce both the Rust-correct value and the CLI's actual value.
  const rustFallback = expectedFundingPayment(market, posLong, idx!, currentRate, nowSlot);
  assert.equal(rustFallback, 0n, "Rust settle_funding: empty book => 0");
  const cliValue = expectedFundingPayment(market, posLong, 0n, currentRate, nowSlot);
  assert.notEqual(cliValue, 0n, "CLI 'mark ?? 0n' path diverges: empty-book funding != 0");
});
