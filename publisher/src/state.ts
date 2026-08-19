/** Decoding of the on-chain `YieldOracle` account (anchor layout). */

/**
 * `YieldOracle` borsh layout (after the 8-byte anchor discriminator):
 *   apy: u64 (8), version: u64 (8), last_update_slot: u64 (8),
 *   publisher: Pubkey (32), authority: Pubkey (32),
 *   stale_after_slots: u64 (8), bump: u8 (1)
 */
const DISCRIMINATOR = 8;
const APY_OFFSET = DISCRIMINATOR;
const VERSION_OFFSET = APY_OFFSET + 8;
const LAST_UPDATE_SLOT_OFFSET = VERSION_OFFSET + 8;
const STALE_AFTER_SLOTS_OFFSET = LAST_UPDATE_SLOT_OFFSET + 8 + 32 + 32;

export interface OracleState {
  apy: bigint;
  version: bigint;
  last_update_slot: bigint;
  stale_after_slots: bigint;
}

export function decodeOracle(data: Buffer | null): OracleState | null {
  if (!data || data.length < STALE_AFTER_SLOTS_OFFSET + 8) {
    return null;
  }
  return {
    apy: data.readBigUInt64LE(APY_OFFSET),
    version: data.readBigUInt64LE(VERSION_OFFSET),
    last_update_slot: data.readBigUInt64LE(LAST_UPDATE_SLOT_OFFSET),
    stale_after_slots: data.readBigUInt64LE(STALE_AFTER_SLOTS_OFFSET),
  };
}

/**
 * Mirror of the on-chain `is_stale` predicate:
 * `current_slot.saturating_sub(last_update_slot) >= stale_after_slots`.
 *
 * `u64` inputs arrive as `bigint`; the comparison is saturation-safe so a
 * `current_slot` behind `last_update_slot` (fork/reorg) reads as zero elapsed.
 */
export function isStale(
  lastUpdateSlot: bigint,
  staleAfterSlots: bigint,
  currentSlot: bigint,
): boolean {
  const elapsed = currentSlot >= lastUpdateSlot ? currentSlot - lastUpdateSlot : 0n;
  return elapsed >= staleAfterSlots;
}
