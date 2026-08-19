/** Jito validator API client — fetches the latest jitoSOL stake-pool APY. */

export interface ApyPoint {
  data: number;
  date: string;
}

export interface StakePoolStats {
  apy?: ApyPoint[];
  [key: string]: unknown;
}

/** On-chain APY scale and ceiling: `1.0 == APY_SCALE`, `100% == APY_SCALE`. */
export const APY_SCALE = 1_000_000;

/**
 * Fetch the latest jitoSOL APY as a decimal (e.g. `0.0718` == 7.18%).
 *
 * Uses Jito's `stake_pool_stats` endpoint, which returns an `apy` time series;
 * the last entry is the most recent.
 */
export async function fetchLatestApy(baseUrl: string): Promise<number> {
  const res = await fetch(`${baseUrl}/api/v1/stake_pool_stats`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ bucket_type: "Daily" }),
  });
  if (!res.ok) {
    throw new Error(`Jito API returned ${res.status}`);
  }
  const json = (await res.json()) as StakePoolStats;
  const series = json.apy;
  if (!series || series.length === 0) {
    throw new Error("Jito API returned no APY data");
  }
  return series[series.length - 1].data;
}

/**
 * Convert a decimal APY (e.g. `0.0718`) to the on-chain scaled u64 (`71800`).
 *
 * Clamped to the on-chain `[0, APY_SCALE]` range so a transient bad API
 * response can never throw in `writeBigUInt64LE` (negative / non-finite) or be
 * rejected on-chain with `ApyTooHigh` (>100%): negative and non-finite inputs
 * map to `0`, values above 100% are capped at `APY_SCALE`.
 */
export function toScaledApy(apyDecimal: number): bigint {
  if (!Number.isFinite(apyDecimal)) {
    return 0n;
  }
  const scaled = Math.round(apyDecimal * APY_SCALE);
  if (scaled < 0) return 0n;
  if (scaled > APY_SCALE) return BigInt(APY_SCALE);
  return BigInt(scaled);
}
