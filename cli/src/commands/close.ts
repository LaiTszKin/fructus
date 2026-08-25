//! `close` — build a `close_position` instruction, optionally chained with a
//! `settle_close` to realize PnL (R-C1).

import type { ParsedArgs } from "../args.js";
import { flagBool, flagStr } from "../args.js";
import type { TraderConfig } from "../env.js";
import { die } from "../errors.js";
import type { DryRunReport } from "../report.js";
import { instructionStep } from "../report.js";
import { parseSide, parseUsize } from "../units.js";
import { derived, emptyParts, placeholders, value } from "./common.js";
import {
  buildClosePosition,
  buildSettleClose,
  orderBookPda,
  positionPda,
  userCollateralPda,
} from "fructus-sdk/src/index.js";

export const usage = `close <side> <size> [options]
  side  long|short (or bid|ask)
  size  notional to close (raw base units)
options:
  --settle                 also build a settle_close instruction (realizes PnL)
  --market <pubkey>        market address (default: derived PDA)
  --program-id <pubkey>    program id
  --submit                 sign + submit on-chain
  --index-source <pubkey>  index (stake pool) source`;

export async function build(cfg: TraderConfig, args: ParsedArgs): Promise<DryRunReport> {
  const sideRaw =
    args.positionals[0] ?? flagStr(args.flags, "side") ?? die("missing <side>");
  const sizeRaw =
    args.positionals[1] ?? flagStr(args.flags, "size") ?? die("missing <size>");

  const side = parseSide(sideRaw);
  const size = parseUsize(sizeRaw, "size");
  const settle = flagBool(args.flags, "settle");

  const ix = buildClosePosition({
    owner: cfg.owner,
    market: cfg.market,
    indexSource: cfg.indexSource,
    side,
    size,
    programId: cfg.programId,
  });
  const instructions = [instructionStep("close_position", ix)];

  const position = positionPda(cfg.market, cfg.owner, side, cfg.programId).address;
  const userCollateral = userCollateralPda(cfg.market, cfg.owner, cfg.programId).address;

  if (settle) {
    instructions.push(
      instructionStep(
        "settle_close",
        buildSettleClose({
          market: cfg.market,
          position,
          userCollateral,
          indexSource: cfg.indexSource,
          programId: cfg.programId,
        }),
      ),
    );
  }

  const parts = emptyParts();
  parts.derived.push(
    derived("position", position, "position"),
    derived("user_collateral", userCollateral, "user_collateral"),
    derived("order_book", orderBookPda(cfg.market, cfg.programId).address, "order_book"),
  );
  parts.values.push(value("side", side === 0 ? "long" : "short"), value("size", size));
  if (settle) {
    parts.values.push(value("close_and_settle", "yes"));
  }
  placeholders(cfg, parts);

  return {
    command: "close",
    mode: cfg.network || cfg.submit ? "network" : "offline",
    programId: cfg.programId,
    market: cfg.market,
    owner: cfg.owner,
    instructions,
    ...parts,
  };
}
