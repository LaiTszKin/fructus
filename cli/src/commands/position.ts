//! `position` — query a position account (offline shows the derived PDA; network
//! mode fetches + decodes it and computes unrealized PnL) (R-C1).

import type { ParsedArgs } from "../args.js";
import { flagStr } from "../args.js";
import { connect, type TraderConfig } from "../env.js";
import { PLACEHOLDER_PUBKEY } from "../env.js";
import { die } from "../errors.js";
import type { DryRunReport } from "../report.js";
import { parseSide } from "../units.js";
import { derived, emptyParts, placeholders } from "./common.js";
import {
  decodePosition,
  pnl,
  positionPda,
  positionSideFromSideByte,
  readExchangeRate,
} from "fructus-sdk/src/index.js";

export const usage = `position <side> [options]
  side  long|short (or bid|ask)
options:
  --network/-n   fetch + decode the live position from the chain
  --market <pubkey>    market address
  --program-id <pubkey> program id
  --index-source <pubkey> index (stake pool) source`;

export async function build(cfg: TraderConfig, args: ParsedArgs): Promise<DryRunReport> {
  const sideRaw =
    args.positionals[0] ?? flagStr(args.flags, "side") ?? die("missing <side>");
  const side = parseSide(sideRaw);
  const position = positionPda(cfg.market, cfg.owner, side, cfg.programId).address;

  const parts = emptyParts();
  parts.derived.push(derived("position", position, "position"));
  parts.values.push({
    label: "unrealized_pnl",
    value: "requires --network",
    note: "fetch + index source to compute",
  });
  placeholders(cfg, parts);

  if (cfg.network) {
    parts.values.pop(); // drop the placeholder pnl note once we can compute
    const conn = connect(cfg);
    const acc = await conn.getAccountInfo(position);
    const pos = decodePosition(acc?.data ?? null);
    if (!pos) {
      parts.warnings.push("position account not found or too short on-chain");
    } else {
      parts.values.push(
        { label: "owner", value: pos.owner.toBase58() },
        { label: "side", value: pos.side === 0 ? "long" : "short" },
        { label: "notional", value: pos.notional.toString() },
        { label: "entry_rate", value: `${pos.entryN}/${pos.entryD}` },
        { label: "collateral", value: pos.collateral.toString() },
        { label: "closed_notional", value: pos.closedNotional.toString() },
        { label: "last_funding_epoch", value: pos.lastFundingEpoch.toString() },
        { label: "open_slot", value: pos.openSlot.toString() },
      );

      if (!cfg.indexSource.equals(PLACEHOLDER_PUBKEY)) {
        const pool = await conn.getAccountInfo(cfg.indexSource);
        const rate = readExchangeRate(pool?.data ?? Buffer.alloc(0));
        if (rate) {
          const ps = positionSideFromSideByte(pos.side);
          const p = ps === null
            ? null
            : pnl(
                pos.entryN,
                pos.entryD,
                rate.totalLamports,
                rate.poolTokenSupply,
                pos.notional,
                ps,
              );
          if (p === null) {
            parts.warnings.push("unrealized PnL is undefined for this position/rate");
          } else {
            parts.values.push({ label: "unrealized_pnl", value: p.toString(), note: "vs current index" });
          }
        } else {
          parts.warnings.push("index source pool rate is not readable");
        }
      } else {
        parts.warnings.push("INDEX_SOURCE not set; cannot price unrealized PnL");
      }
    }
  }

  return {
    command: "position",
    mode: cfg.network || cfg.submit ? "network" : "offline",
    programId: cfg.programId,
    market: cfg.market,
    owner: cfg.owner,
    instructions: [],
    ...parts,
  };
}
