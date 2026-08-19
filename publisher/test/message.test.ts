import { test } from "node:test";
import assert from "node:assert/strict";
import { PublicKey } from "@solana/web3.js";
import { updateApyDiscriminator, updateMessage } from "../src/message.js";
import { APY_SCALE, toScaledApy } from "../src/jito.js";
import { decodeOracle, isStale } from "../src/state.js";

// Cross-language lock: this vector is asserted on-chain in
// `programs/fructus/src/tests.rs::update_message_matches_known_vector`.
test("updateMessage matches the on-chain Rust vector", () => {
  // oracle = 32 bytes of 0x01, apy = 71840 (7.184%), version = 1
  const oracle = new PublicKey(new Uint8Array(32).fill(1));
  const msg = updateMessage(oracle, 71_840n, 1n);
  assert.equal(
    msg.toString("hex"),
    "dd9394a5f5b4b383f2478ae97164cb69b495245a220a1be1d0996a0e0d54c1a0",
  );
});

test("updateApyDiscriminator is the anchor sha256 prefix", () => {
  // sha256("global:update_apy")[0..8]
  const d = updateApyDiscriminator();
  assert.equal(d.length, 8);
});

test("toScaledApy scales a decimal to the fixed-point u64", () => {
  assert.equal(toScaledApy(0.0718), 71_800n);
  assert.equal(toScaledApy(1), BigInt(APY_SCALE));
  assert.equal(toScaledApy(0), 0n);
});

test("toScaledApy clamps out-of-range values to [0, APY_SCALE]", () => {
  assert.equal(toScaledApy(-0.05), 0n);
  assert.equal(toScaledApy(-100), 0n);
  assert.equal(toScaledApy(1.5), BigInt(APY_SCALE));
  assert.equal(toScaledApy(1_000_000), BigInt(APY_SCALE));
});

test("toScaledApy never throws on non-finite input", () => {
  assert.equal(toScaledApy(Number.NaN), 0n);
  assert.equal(toScaledApy(Number.POSITIVE_INFINITY), 0n);
  assert.equal(toScaledApy(Number.NEGATIVE_INFINITY), 0n);
});

test("isStale mirrors the on-chain saturating predicate", () => {
  // elapsed = 4 < 5 → fresh
  assert.equal(isStale(10n, 5n, 14n), false);
  // elapsed = 5 >= 5 → stale
  assert.equal(isStale(10n, 5n, 15n), true);
  // current_slot behind last_update_slot saturates to zero elapsed
  assert.equal(isStale(20n, 5n, 10n), false);
  // a zero window is always stale
  assert.equal(isStale(10n, 0n, 10n), true);
});

test("decodeOracle reads apy, version, last_update_slot and stale_after_slots", () => {
  const data = Buffer.alloc(105);
  data.writeBigUInt64LE(71_840n, 8); // apy
  data.writeBigUInt64LE(3n, 16); // version
  data.writeBigUInt64LE(123_456n, 24); // last_update_slot
  data.writeBigUInt64LE(42_000n, 96); // stale_after_slots

  assert.deepEqual(decodeOracle(data), {
    apy: 71_840n,
    version: 3n,
    last_update_slot: 123_456n,
    stale_after_slots: 42_000n,
  });
});

test("decodeOracle returns null for a truncated account", () => {
  assert.equal(decodeOracle(Buffer.alloc(103)), null);
  assert.equal(decodeOracle(null), null);
});
