//! `index` — compute the trustless on-chain index level (R-C1).

import type { ParsedArgs } from "../args.js";
import { flagStr } from "../args.js";
import { connect, type TraderConfig } from "../env.js";
import { PLACEHOLDER_PUBKEY } from "../env.js";
import { die } from "../errors.js";
import type { DryRunReport } from "../report.js";
import { parseInt, parseUsize } from "../units.js";
import { derived, emptyParts, placeholders, value } from "./common.js";
import {
  decodePerpMarket,
  expectedIndex,
  fundingEpoch,
  readExchangeRate,
} from "fructus-sdk/src/index.js";

export const usage = `index [options]
options:
  --network/-n   fetch market + pool, compute the live index
  --baseline-n <n>  baseline total_lamports (offline)
  --baseline-d <n>  baseline pool_token_supply (offline)
  --current-n <n>   current total_lamports (offline)
  --current-d <n>   current pool_token_supply (offline)
  --elapsed-slots <n> elapsed slots (offline)
  --market <pubkey>   market address
  --program-id <pubkey> program id
  --index-source <pubkey> index (stake pool) source`;

export async function build(cfg: TraderConfig, args: ParsedArgs): Promise<DryRunReport> {
  const parts = emptyParts();
  parts.derived.push(
    derived("market", cfg.market, "perp_market"),
    derived("index_source", cfg.indexSource, "index_source"),
  );
  placeholders(cfg, parts);

  if (cfg.network) {
    const conn = connect(cfg);
    const marketAcc = await conn.getAccountInfo(cfg.market);
    const market = decodePerpMarket(marketAcc?.data ?? null);
    if (!market) {
      parts.warnings.push("market account not found or too short on-chain");
      parts.values.push({ label: "index", value: "n/a" });
    } else {
      const nowSlot = BigInt(await conn.getSlot());
      const curEpoch = fundingEpoch(nowSlot, market.fundingEpochSlots);
      const elapsed = (curEpoch - market.fundingEpoch) * market.fundingEpochSlots;
      const baseline =
        market.indexD === 0n
          ? null
          : { totalLamports: market.indexN, poolTokenSupply: market.indexD };

      if (cfg.indexSource.equals(PLACEHOLDER_PUBKEY) && market.indexD === 0n) {
        parts.values.push({ label: "index", value: "0", note: "no baseline set on market" });
      } else if (cfg.indexSource.equals(PLACEHOLDER_PUBKEY)) {
        parts.warnings.push("INDEX_SOURCE not set; cannot read the current pool rate");
        parts.values.push({ label: "index", value: "n/a" });
      } else {
        const pool = await conn.getAccountInfo(cfg.indexSource);
        const current = readExchangeRate(pool?.data ?? Buffer.alloc(0));
        if (!current) {
          parts.warnings.push("index source pool rate is not readable");
          parts.values.push({ label: "index", value: "n/a" });
        } else {
          const idx = expectedIndex(baseline, current, elapsed);
          parts.values.push({
            label: "index",
            value: idx === null ? "n/a" : idx.toString(),
            note: `elapsed=${elapsed} slots`,
          });
          parts.values.push({ label: "index_n/index_d", value: `${market.indexN}/${market.indexD}` });
        }
      }
    }
  } else {
    const bN = flagStr(args.flags, "baseline-n");
    const bD = flagStr(args.flags, "baseline-d");
    const cN = flagStr(args.flags, "current-n");
    const cD = flagStr(args.flags, "current-d");
    const eS = flagStr(args.flags, "elapsed-slots");
    if (bN && bD && cN && cD && eS) {
      const idx = expectedIndex(
        { totalLamports: parseInt(bN, "baseline-n"), poolTokenSupply: parseInt(bD, "baseline-d") },
        { totalLamports: parseInt(cN, "current-n"), poolTokenSupply: parseInt(cD, "current-d") },
        parseUsize(eS, "elapsed-slots"),
      );
      parts.values.push({ label: "index", value: idx === null ? "n/a" : idx.toString() });
    } else {
      parts.values.push({ label: "index", value: "requires --network (or supply baseline/current/elapsed flags)" });
    }
  }

  return {
    command: "index",
    mode: cfg.network || cfg.submit ? "network" : "offline",
    programId: cfg.programId,
    market: cfg.market,
    owner: cfg.owner,
    instructions: [],
    ...parts,
  };
}
