import { test } from "node:test";
import assert from "node:assert/strict";
import { PublicKey } from "@solana/web3.js";
import { TXV1_GATE_ID } from "../src/v1.js";

// Regression guard for the defect the plan shipped: the SIMD-0385 feature-gate literal
// (`txv1aq4pp281K9um3pgkfX8UqtFT6wcVW3hNezGLL`), copied from issue #18's text into the
// design, the stub and the frozen test, was 41 characters — not a valid base58 32-byte
// pubkey. `new PublicKey(...)` threw `Invalid public key input`, the RPC answered
// `Invalid param: WrongSize`, and no client could ever address the feature account, so
// the v1 path could never have worked on a live cluster. `v1-gate.test.ts` pins the
// constant to its corrected value; the assertions here guard the CLASS of defect — the
// constant must stay addressable, canonical and correctly sized.
//
// Measurements behind the correction: `.plan/20260924/stage2-deps-and-v1/AMENDMENTS.md`
// (`solana feature status --url devnet | grep SIMD-0385` and the two getAccountInfo calls).

/** The 41-character literal that shipped in the plan (kept here as the negative case). */
const PLAN_TYPO_LITERAL = "txv1aq4pp281K9um3pgkfX8UqtFT6wcVW3hNezGLL";

test("V1-GATE-ID-IS-ADDRESSABLE: the gate constant constructs as a 32-byte base58 pubkey", () => {
  const gate = new PublicKey(TXV1_GATE_ID); // throws on the plan's 41-character literal
  assert.equal(gate.toBytes().length, 32);
  assert.equal(gate.toBase58(), TXV1_GATE_ID, "the literal is canonical base58, not an alias");
  assert.match(TXV1_GATE_ID, /^[1-9A-HJ-NP-Za-km-z]+$/, "base58 alphabet only");
});

test("V1-GATE-ID-REJECTS-THE-TYPO: the old 41-character literal is not a pubkey at all", () => {
  assert.notEqual(TXV1_GATE_ID, PLAN_TYPO_LITERAL);
  assert.throws(() => new PublicKey(PLAN_TYPO_LITERAL), /Invalid public key input/);
});
