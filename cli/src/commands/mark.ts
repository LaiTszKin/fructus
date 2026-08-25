//! `mark` — compute the order-book mark (mid) and TWAP (R-C1).

import type { ParsedArgs } from "../args.js";
import { flagStr } from "../args.js";
import { connect, type TraderConfig } from "../env.js";
import { die } from "../errors.js";
import type { DryRunReport } from "../report.js";
import { parseInt, parseUsize } from "../units.js";
import { derived, emptyParts, placeholders, value } from "./common.js";
import {
  decodeOrderBook,
  mid,
  midFromBook,
  orderBookPda,
  twapFromBook,
} from "fructus-sdk/src/index.js";

export const usage = `mark [options]
options:
  --network/-n       fetch + decode the order book, print mid + TWAP
  --bid <n>          best bid price (offline mid computation)
  --ask <n>          best ask price (offline mid computation)
  --twap-window <n>  TWAP window slots (default 16)
  --market <pubkey>  market address
  --program-id <pubkey> program id`;

export async function build(cfg: TraderConfig, args: ParsedArgs): Promise<DryRunReport> {
  const orderBook = orderBookPda(cfg.market, cfg.programId).address;
  const parts = emptyParts();
  parts.derived.push(derived("order_book", orderBook, "order_book"));
  placeholders(cfg, parts);

  if (cfg.network) {
    const conn = connect(cfg);
    const acc = await conn.getAccountInfo(orderBook);
    const book = decodeOrderBook(acc?.data ?? null);
    if (!book) {
      parts.warnings.push("order book account not found or too short on-chain");
      parts.values.push({ label: "mark", value: "n/a" });
    } else {
      const m = midFromBook(book);
      parts.values.push({
        label: "mark_mid",
        value: m === null ? "n/a (one-sided book)" : m.toString(),
      });
      const windowRaw = flagStr(args.flags, "twap-window");
      const window = windowRaw ? parseUsize(windowRaw, "twap-window") : 16n;
      const nowSlot = BigInt(await conn.getSlot());
      const t = twapFromBook(book, window, nowSlot);
      parts.values.push({
        label: "mark_twap",
        value: t === null ? "n/a (short history)" : t.toString(),
        note: `window=${window} slots`,
      });
    }
  } else {
    const bidRaw = flagStr(args.flags, "bid");
    const askRaw = flagStr(args.flags, "ask");
    if (bidRaw && askRaw) {
      const bid = parseInt(bidRaw, "bid");
      const ask = parseInt(askRaw, "ask");
      const m = mid(bid, ask);
      parts.values.push({ label: "mark_mid", value: m === null ? "n/a" : m.toString() });
    } else {
      parts.values.push({ label: "mark_mid", value: "requires --network (or --bid/--ask)" });
    }
  }

  return {
    command: "mark",
    mode: cfg.network || cfg.submit ? "network" : "offline",
    programId: cfg.programId,
    market: cfg.market,
    owner: cfg.owner,
    instructions: [],
    ...parts,
  };
}
