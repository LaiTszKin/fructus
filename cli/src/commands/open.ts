//! `open` — build an `open_position` instruction (R-C1).

import type { ParsedArgs } from "../args.js";
import { flagStr } from "../args.js";
import type { TraderConfig } from "../env.js";
import { PLACEHOLDER_PUBKEY } from "../env.js";
import { die } from "../errors.js";
import type { DryRunReport } from "../report.js";
import { instructionStep } from "../report.js";
import { parseSide, parseUsize } from "../units.js";
import { derived, emptyParts, placeholders, value } from "./common.js";
import {
  buildOpenPosition,
  marginRequired,
  orderBookPda,
  positionPda,
  userCollateralPda,
} from "fructus-sdk/src/index.js";

export const usage = `open <side> <size> <price> [options]
  side  long|short (or bid|ask)
  size  position size (raw base units)
  price entry price / yield level (APY_SCALE fixed-point)
options:
  --initial-margin-bps <n>   include estimated initial margin
  --market <pubkey>          market address (default: derived PDA)
  --program-id <pubkey>      program id (default: env PROGRAM_ID)
  --submit                   sign + submit on-chain
  --index-source <pubkey>    index (stake pool) source`;

export async function build(cfg: TraderConfig, args: ParsedArgs): Promise<DryRunReport> {
  const sideRaw =
    args.positionals[0] ?? flagStr(args.flags, "side") ?? die("missing <side>");
  const sizeRaw =
    args.positionals[1] ?? flagStr(args.flags, "size") ?? die("missing <size>");
  const priceRaw =
    args.positionals[2] ?? flagStr(args.flags, "price") ?? die("missing <price>");

  const side = parseSide(sideRaw);
  const size = parseUsize(sizeRaw, "size");
  const price = parseUsize(priceRaw, "price");

  const ix = buildOpenPosition({
    owner: cfg.owner,
    market: cfg.market,
    indexSource: cfg.indexSource,
    side,
    size,
    price,
    programId: cfg.programId,
  });

  const parts = emptyParts();
  parts.derived.push(
    derived("position", positionPda(cfg.market, cfg.owner, side, cfg.programId).address, "position"),
    derived("user_collateral", userCollateralPda(cfg.market, cfg.owner, cfg.programId).address, "user_collateral"),
    derived("order_book", orderBookPda(cfg.market, cfg.programId).address, "order_book"),
  );

  const notional = size * price;
  parts.values.push(value("side", side === 0 ? "long" : "short"), value("size", size), value("price", price), value("notional", notional));
  const bpsRaw = flagStr(args.flags, "initial-margin-bps");
  if (bpsRaw) {
    const bps = Number(bpsRaw);
    parts.values.push(value("initial_margin", marginRequired(notional, bps), `${bps} bps`));
  }
  placeholders(cfg, parts);

  const report: DryRunReport = {
    command: "open",
    mode: cfg.network || cfg.submit ? "network" : "offline",
    programId: cfg.programId,
    market: cfg.market,
    owner: cfg.owner,
    instructions: [instructionStep("open_position", ix)],
    ...parts,
  };

  if (cfg.indexSource.equals(PLACEHOLDER_PUBKEY)) {
    report.warnings.push("expected funding cannot be priced without INDEX_SOURCE");
  }
  return report;
}
