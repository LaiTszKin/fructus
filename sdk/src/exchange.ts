//! Trustless settlement mirror of `programs/fructus/src/exchange.rs` + the
//! stake-pool account offsets. Reads the JitoSOL exchange rate from a raw pool
//! account and derives realized/annualized yield — the trustless `index` source.

import { APY_SCALE } from "./constants.js";

/** Byte offset of the borsh `AccountType` discriminator in the `StakePool` account. */
export const ACCOUNT_TYPE_OFFSET = 0;

/** `AccountType::StakePool` borsh discriminator (byte 0 of a live pool account). */
export const ACCOUNT_TYPE_STAKE_POOL = 1;

/** Byte offset of `total_lamports` in the SPL Stake Pool `StakePool` account. */
export const TOTAL_LAMPORTS_OFFSET = 258;

/** Byte offset of `pool_token_supply` in the SPL Stake Pool `StakePool` account. */
export const POOL_TOKEN_SUPPLY_OFFSET = 266;

/** A rational exchange rate (SOL per jitoSOL), kept as a numerator/denominator pair. */
export interface ExchangeRate {
  totalLamports: bigint;
  poolTokenSupply: bigint;
}

/**
 * Read the exchange rate from raw stake-pool account data. Returns `null` unless
 * the account carries the `StakePool` discriminator, the data is long enough to
 * contain both fields, or the token supply is nonzero.
 */
export function readExchangeRate(data: Buffer): ExchangeRate | null {
  if (data[ACCOUNT_TYPE_OFFSET] !== ACCOUNT_TYPE_STAKE_POOL) {
    return null;
  }
  if (data.length < POOL_TOKEN_SUPPLY_OFFSET + 8) {
    return null;
  }
  const totalLamports = data.readBigUInt64LE(TOTAL_LAMPORTS_OFFSET);
  const poolTokenSupply = data.readBigUInt64LE(POOL_TOKEN_SUPPLY_OFFSET);
  if (poolTokenSupply === 0n) {
    return null;
  }
  return { totalLamports, poolTokenSupply };
}

/**
 * Realized yield from `open` (t0) to `settle` (t1), scaled by `APY_SCALE`:
 * `(rate_t1 / rate_t0 - 1) × APY_SCALE`. A negative yield clamps to `0`. Returns
 * `null` on a zero numerator/denominator.
 */
export function realizedYield(open: ExchangeRate, settle: ExchangeRate): bigint | null {
  if (settle.totalLamports === 0n || open.totalLamports === 0n) {
    return null;
  }
  // rate_t1 / rate_t0 = (n1·d0) / (n0·d1)
  const a = settle.totalLamports * open.poolTokenSupply;
  const b = open.totalLamports * settle.poolTokenSupply;
  if (b === 0n) {
    return null;
  }
  if (a < b) {
    // Negative yield cannot happen for a functioning LST; clamp defensively.
    return 0n;
  }
  const diff = a - b;
  return (diff * APY_SCALE) / b;
}

/**
 * Annualize a scaled yield over `periodSlots` elapsed slots:
 * `yieldScaled × slotsPerYear / periodSlots`. Returns `null` for a zero period.
 */
export function annualize(
  yieldScaled: bigint,
  periodSlots: bigint,
  slotsPerYear: bigint,
): bigint | null {
  if (periodSlots === 0n) {
    return null;
  }
  return (yieldScaled * slotsPerYear) / periodSlots;
}
