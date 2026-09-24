import { test } from "node:test";
import assert from "node:assert/strict";
import { createPublicKey, verify as verifyEd25519 } from "node:crypto";
import { Ed25519Program, Keypair, PublicKey } from "@solana/web3.js";
import { updateApyDiscriminator, updateMessage, writeU64LE } from "../src/message.js";
import { buildUpdateApyIx, buildUpdateTx } from "../src/update.js";

// Regression: `buildUpdateTx` used to call `Keypair.sign`, which @solana/web3.js >=
// 1.98 no longer exposes — neither in its types nor at run time
// (`TypeError: publisher.sign is not a function`). `tsx --test` strips types and the
// old suite never executed this path, so every test stayed green while the keeper
// could not build a transaction at all. These tests both call the path and check the
// bytes it produces.

/** DER prefix of an Ed25519 SubjectPublicKeyInfo (RFC 8410) — for node:crypto. */
const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

const ORACLE = new PublicKey(new Uint8Array(32).fill(7));
const PROGRAM_ID = new PublicKey(new Uint8Array(32).fill(9));
const PUBLISHER = Keypair.generate();
const APY = 71_840n;
const VERSION = 7n;

/** Parse the ed25519 verify instruction the SDK emits and return its parts. */
function parseEd25519Instruction(data: Uint8Array) {
  const d = Buffer.from(data);
  assert.equal(d.readUInt8(0), 1, "exactly one signature");
  assert.equal(d.readUInt8(1), 0, "padding byte is zero");
  const signatureOffset = d.readUInt16LE(2);
  const signatureInstructionIndex = d.readUInt16LE(4);
  const publicKeyOffset = d.readUInt16LE(6);
  const publicKeyInstructionIndex = d.readUInt16LE(8);
  const messageOffset = d.readUInt16LE(10);
  const messageSize = d.readUInt16LE(12);
  const messageInstructionIndex = d.readUInt16LE(14);
  for (const idx of [signatureInstructionIndex, publicKeyInstructionIndex, messageInstructionIndex]) {
    assert.equal(idx, 0xffff, "offsets point into this instruction's own data");
  }
  return {
    signature: d.subarray(signatureOffset, signatureOffset + 64),
    publicKey: d.subarray(publicKeyOffset, publicKeyOffset + 32),
    message: d.subarray(messageOffset, messageOffset + messageSize),
  };
}

test("buildUpdateTx builds a signed update without Keypair.sign (ed25519 verify + update_apy)", () => {
  const tx = buildUpdateTx({ oracle: ORACLE, programId: PROGRAM_ID, publisher: PUBLISHER, apy: APY, version: VERSION });

  assert.equal(tx.instructions.length, 2, "ed25519 verify + update_apy");
  const [ed25519Ix, updateIx] = tx.instructions;

  // 1. the verify instruction targets the ed25519 program and carries a VALID signature
  assert.ok(ed25519Ix.programId.equals(Ed25519Program.programId));
  const { signature, publicKey, message } = parseEd25519Instruction(ed25519Ix.data);
  assert.deepEqual(Buffer.from(publicKey), Buffer.from(PUBLISHER.publicKey.toBytes()));
  assert.deepEqual(Buffer.from(message), updateMessage(ORACLE, APY, VERSION));
  const spki = Buffer.concat([ED25519_SPKI_PREFIX, Buffer.from(publicKey)]);
  assert.ok(
    verifyEd25519(null, Buffer.from(message), createPublicKey({ key: spki, format: "der", type: "spki" }), Buffer.from(signature)),
    "the ed25519 signature verifies against the canonical update message",
  );

  // 2. the update instruction pairs it with the on-chain program call
  assert.ok(updateIx.programId.equals(PROGRAM_ID));
  assert.deepEqual(
    Buffer.from(updateIx.data),
    Buffer.concat([updateApyDiscriminator(), writeU64LE(APY), writeU64LE(VERSION)]),
  );
  assert.deepEqual(
    updateIx.keys.map((k) => ({ pubkey: k.pubkey.toBase58(), isSigner: k.isSigner, isWritable: k.isWritable })),
    [
      { pubkey: ORACLE.toBase58(), isSigner: false, isWritable: true },
      { pubkey: "Sysvar1nstructions1111111111111111111111111", isSigner: false, isWritable: false },
    ],
  );

  // 3. the publisher pays for it
  assert.ok(tx.feePayer?.equals(PUBLISHER.publicKey));

  // 4. buildUpdateApyIx alone is pure and unchanged
  const bare = buildUpdateApyIx({ oracle: ORACLE, programId: PROGRAM_ID, publisher: PUBLISHER, apy: APY, version: VERSION });
  assert.deepEqual(Buffer.from(bare.data), Buffer.from(updateIx.data));
});
