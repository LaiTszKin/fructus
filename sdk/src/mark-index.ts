//! Mark/index query helpers (R-SDK3): compute the trustless on-chain `index`,
//! the order-book `mark`, and the **expected funding payment** a position would
//! accrue — mirroring the `settle_funding` adapter in `lib.rs` plus the pure
//! `funding.rs` / `exchange.rs` / `orderbook.rs` math.

import type { PerpMarketState, PositionState, OrderBookState } from "./account/decode.js";
import { SLOTS_PER_YEAR } from "./constants.js";
import { fundingEpoch, fundingPayment, fundingRate, premium, SideFlow } from "./funding.js";
import { annualize, realizedYield } from "./exchange.js";
import { mid, twap } from "./orderbook.js";

/**
 * The trustless on-chain index level: `annualize(realized_yield(baseline,
 * current), elapsedSlots, SLOTS_PER_YEAR)`, where the baseline is the market's
 * last-settlement `index_n/index_d` snapshot. `0` when no baseline is set
 * (`index_d == 0`), matching the first `settle_funding`.
 */
export function expectedIndex(
  baseline: { totalLamports: bigint; poolTokenSupply: bigint } | null,
  current: { totalLamports: bigint; poolTokenSupply: bigint },
  elapsedSlots: bigint,
): bigint | null {
  if (!baseline) {
    return 0n;
  }
  if (baseline.poolTokenSupply === 0n) {
    return 0n;
  }
  const realized = realizedYield(baseline, current);
  if (realized === null) {
    return null;
  }
  return annualize(realized, elapsedSlots, SLOTS_PER_YEAR);
}

/** The order-book mid for a decoded book; `null` when either side is empty. */
export function midFromBook(book: OrderBookState): bigint | null {
  return mid(book.bestBid, book.bestAsk);
}

/** The order-book TWAP over the decoded observation ring; `null` on a short history. */
export function twapFromBook(
  book: OrderBookState,
  windowSlots: bigint,
  nowSlot: bigint,
): bigint | null {
  return twap(
    book.observations.map((o) => ({ slot: o.slot, cumulativeMid: o.cumulativeMid })),
    windowSlots,
    nowSlot,
  );
}

/**
 * The **expected funding payment** for a position, mirroring `settle_funding`:
 * derive the elapsed full epochs, compute the trustless index, price the
 * `mark - index` premium, clamp the rate, and apply the signed
 * notional·rate·epochs·side_flow payment.
 *
 * `mark` is the order-book mid (pass `midFromBook(book)`); when the book is
 * one-sided/empty pass `index` so `premium == 0`. `currentRate` is the live
 * pool exchange rate; `nowSlot` is the current slot.
 */
export function expectedFundingPayment(
  market: PerpMarketState,
  position: PositionState,
  mark: bigint,
  currentRate: { totalLamports: bigint; poolTokenSupply: bigint },
  nowSlot: bigint,
): bigint | null {
  const curEpoch = fundingEpoch(nowSlot, market.fundingEpochSlots);
  const epochs = curEpoch - position.lastFundingEpoch;
  if (epochs <= 0n) {
    return 0n;
  }
  const elapsedSlots = epochs * market.fundingEpochSlots;

  const index = expectedIndex(
    market.indexD === 0n
      ? null
      : { totalLamports: market.indexN, poolTokenSupply: market.indexD },
    currentRate,
    elapsedSlots,
  );
  if (index === null) {
    return null;
  }

  const p = premium(mark, index);
  const rate = fundingRate(p, market.fundingK, market.maxFunding);
  const flow = position.side === 0 ? SideFlow.Long : SideFlow.Short;
  return fundingPayment(position.notional, rate, epochs, flow);
}
