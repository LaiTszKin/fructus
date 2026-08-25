//! Pure order-book mark helpers mirroring `programs/fructus/src/orderbook.rs`:
//! best bid/ask, the order-book mid, and the time-weighted-average-mid (the
//! liquidation reference price). These are the "mark" half of the mark/index
//! query helpers.

export interface Observation {
  slot: bigint;
  /** Running `Σ mid × Δslot` accumulator. */
  cumulativeMid: bigint;
}

/** Best (highest) resting bid price, or `0` when the bid side is empty. */
export function bestBid(bids: bigint[]): bigint {
  return bids.reduce((a, b) => (b > a ? b : a), 0n);
}

/** Best (lowest) resting ask price, or `0` when the ask side is empty. */
export function bestAsk(asks: bigint[]): bigint {
  return asks.reduce((a, b) => (b < a ? b : a), 0n);
}

/**
 * Mid price: `(best_bid + best_ask) / 2`, truncating toward zero. Returns `null`
 * iff either side is empty (`best_bid == 0 || best_ask == 0`).
 */
export function mid(bestBidValue: bigint, bestAskValue: bigint): bigint | null {
  if (bestBidValue === 0n || bestAskValue === 0n) {
    return null;
  }
  return (bestBidValue + bestAskValue) / 2n;
}

/**
 * Time-weighted average mid over `windowSlots` ending at `nowSlot`. Computes
 * `(cum_now - cum_then) / windowSlots` from the running `cumulative_mid`
 * accumulator. Returns `null` for a zero window, an empty history, a history
 * that does not reach back a full window, or any `u64` overflow.
 */
export function twap(obs: Observation[], windowSlots: bigint, nowSlot: bigint): bigint | null {
  if (windowSlots === 0n || obs.length === 0) {
    return null;
  }
  const startSlot = nowSlot - windowSlots;
  if (startSlot < 0n) {
    return null;
  }
  const cumNow = cumulativeAt(obs, nowSlot);
  const cumStart = cumulativeAt(obs, startSlot);
  if (cumNow === null || cumStart === null) {
    return null;
  }
  const delta = cumNow - cumStart;
  if (delta < 0n) {
    return null;
  }
  return delta / windowSlots;
}

/**
 * The running `cumulative_mid` at `slot`, interpolated piecewise-linearly
 * between the surrounding observations (mirrors `orderbook::cumulative_at`).
 */
export function cumulativeAt(obs: Observation[], slot: bigint): bigint | null {
  if (obs.length === 0) {
    return null;
  }
  const hit = obs.find((o) => o.slot === slot);
  if (hit) {
    return hit.cumulativeMid;
  }
  const before = obs.filter((o) => o.slot < slot);
  const lo = before.length ? before.reduce((a, b) => (b.slot > a.slot ? b : a)) : null;
  if (!lo) {
    return null;
  }
  const after = obs.filter((o) => o.slot > slot);
  const hi = after.length ? after.reduce((a, b) => (b.slot < a.slot ? b : a)) : null;
  if (hi) {
    return interpolate(lo, hi, slot);
  }
  // `slot` is after the last observation: extrapolate with the trailing mid.
  const prev = obs
    .filter((o) => o.slot < lo.slot)
    .reduce<Observation | null>((acc, o) => (acc === null || o.slot > acc.slot ? o : acc), null);
  if (!prev) {
    return null;
  }
  return interpolate(prev, lo, slot);
}

/** `cumulative_mid` at `slot`, linearly interpolated along the constant-mid segment. */
function interpolate(lo: Observation, hi: Observation, slot: bigint): bigint | null {
  const span = hi.slot - lo.slot;
  if (span <= 0n) {
    return null;
  }
  const offset = slot - lo.slot;
  if (offset < 0n) {
    return null;
  }
  const rise = hi.cumulativeMid - lo.cumulativeMid;
  if (rise < 0n) {
    return null;
  }
  const inc = (rise * offset) / span;
  return lo.cumulativeMid + inc;
}
