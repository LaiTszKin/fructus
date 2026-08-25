//! `funding` — build a `settle_funding` instruction and (network mode) compute the
//! expected funding payment (R-C1).

import type { ParsedArgs } from "../args.js";
import { flagStr } from "../args.js";
import { connect, type TraderConfig } from "../env.js";
import { PLACEHOLDER_PUBKEY } from "../env.js";
import { die } from "../errors.js";
import type { DryRunReport } from "../report.js";
import { instructionStep } from "../report.js";
import { parseInt, parseSide, parseUsize } from "../units.js";
import { derived, emptyParts, placeholders, value } from "./common.js";
import {
  buildSettleFunding,
  decodeOrderBook,
  decodePerpMarket,
  decodePosition,
  expectedFundingPayment,
  expectedIndex,
  fundingEpoch,
  fundingPayment,
  fundingRate,
  midFromBook,
  orderBookPda,
  positionPda,
  readExchangeRate,
  SideFlow,
  userCollateralPda,
  type PerpMarketState,
  type PositionState,
} from "fructus-sdk/src/index.js";

/**
 * The expected funding payment mirroring on-chain `settle_funding`, using the
 * mark fallback exactly as the program does.
 *
 * `mark` is the order-book mid (`midFromBook(book)`); when the book is
 * one-sided/empty it is `null`, and the helper falls back to the **index**
 * (`mark ?? index`), so `premium == index - index == 0` and the payment is
 * `0` — identical to the Rust `mark = orderbook::mid(&book).unwrap_or(index)`.
 * The `?? 0n` term is inert for type-safety (`expectedFundingPayment` returns
 * `null` when the index is un-derivable, regardless of the mark). Extracted so
 * the R-1 mark-fallback invariant is deterministically unit-testable.
 */
export function expectedFundingForBook(
  market: PerpMarketState,
  position: PositionState,
  mark: bigint | null,
  current: { totalLamports: bigint; poolTokenSupply: bigint },
  nowSlot: bigint,
): bigint | null {
  const index = expectedIndex(
    market.indexD === 0n
      ? null
      : { totalLamports: market.indexN, poolTokenSupply: market.indexD },
    current,
    (fundingEpoch(nowSlot, market.fundingEpochSlots) - position.lastFundingEpoch) *
      market.fundingEpochSlots,
  );
  return expectedFundingPayment(market, position, mark ?? index ?? 0n, current, nowSlot);
}

export const usage = `funding <side> [options]
  side  long|short (or bid|ask)
options:
  --network/-n      fetch market/position/book/pool; compute + print expected funding
  --submit          sign + submit the settle_funding instruction
  --notional <n>    offline: position notional
  --premium <n>     offline: mark - index premium (signed)
  --funding-k <n>   offline: funding convergence speed
  --max-funding <n> offline: per-epoch rate cap
  --epochs <n>      offline: elapsed full funding epochs
  --market <pubkey> market address
  --program-id <pubkey> program id
  --index-source <pubkey> index (stake pool) source`;

export async function build(cfg: TraderConfig, args: ParsedArgs): Promise<DryRunReport> {
  const sideRaw =
    args.positionals[0] ?? flagStr(args.flags, "side") ?? die("missing <side>");
  const side = parseSide(sideRaw);

  const orderBook = orderBookPda(cfg.market, cfg.programId).address;
  const position = positionPda(cfg.market, cfg.owner, side, cfg.programId).address;
  const userCollateral = userCollateralPda(cfg.market, cfg.owner, cfg.programId).address;

  const ix = buildSettleFunding({
    market: cfg.market,
    position,
    userCollateral,
    orderBook,
    indexSource: cfg.indexSource,
    programId: cfg.programId,
  });

  const parts = emptyParts();
  parts.derived.push(
    derived("position", position, "position"),
    derived("user_collateral", userCollateral, "user_collateral"),
    derived("order_book", orderBook, "order_book"),
  );
  parts.values.push(value("side", side === 0 ? "long" : "short"));
  placeholders(cfg, parts);

  // Offline expected-funding computation from raw inputs.
  const notionalRaw = flagStr(args.flags, "notional");
  const premiumRaw = flagStr(args.flags, "premium");
  const kRaw = flagStr(args.flags, "funding-k");
  const maxRaw = flagStr(args.flags, "max-funding");
  const epochsRaw = flagStr(args.flags, "epochs");
  if (notionalRaw && premiumRaw && kRaw && maxRaw && epochsRaw) {
    const rate = fundingRate(
      parseInt(premiumRaw, "premium"),
      parseInt(kRaw, "funding-k"),
      parseInt(maxRaw, "max-funding"),
    );
    const flow = side === 0 ? SideFlow.Long : SideFlow.Short;
    const pay = fundingPayment(
      parseUsize(notionalRaw, "notional"),
      rate,
      parseUsize(epochsRaw, "epochs"),
      flow,
    );
    parts.values.push(value("funding_rate", rate));
    parts.values.push(value("expected_funding_payment", pay));
  }

  if (cfg.network) {
    const conn = connect(cfg);
    const marketAcc = await conn.getAccountInfo(cfg.market);
    const market = decodePerpMarket(marketAcc?.data ?? null);
    const posAcc = await conn.getAccountInfo(position);
    const pos = decodePosition(posAcc?.data ?? null);
    if (!market || !pos) {
      parts.warnings.push("market/position account not found or too short on-chain");
    } else {
      const book = decodeOrderBook((await conn.getAccountInfo(orderBook))?.data ?? null);
      const mark = book ? midFromBook(book) : null;
      const current = readExchangeRate(
        (await conn.getAccountInfo(cfg.indexSource))?.data ?? Buffer.alloc(0),
      );
      const nowSlot = BigInt(await conn.getSlot());
      if (!current) {
        parts.warnings.push("index source pool rate is not readable");
      } else {
        const payment = expectedFundingForBook(market, pos, mark, current, nowSlot);
        parts.values.push({
          label: "expected_funding_payment",
          value: payment === null ? "n/a" : payment.toString(),
          note: `${mark === null ? "one-sided book (premium=0)" : `mark=${mark}`}`,
        });
      }
    }
  }

  if (!cfg.network && !(notionalRaw && premiumRaw && kRaw && maxRaw && epochsRaw)) {
    parts.values.push({
      label: "expected_funding_payment",
      value: "requires --network (or notional/premium/funding-k/max-funding/epochs)",
    });
  }

  return {
    command: "funding",
    mode: cfg.network || cfg.submit ? "network" : "offline",
    programId: cfg.programId,
    market: cfg.market,
    owner: cfg.owner,
    instructions: [instructionStep("settle_funding", ix)],
    ...parts,
  };
}
