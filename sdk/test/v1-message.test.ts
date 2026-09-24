import { test } from "node:test";
import assert from "node:assert/strict";
import {
  ComputeBudgetProgram,
  Keypair,
  PublicKey,
  SystemProgram,
  TransactionInstruction,
} from "@solana/web3.js";
import { compileTransactionMessage } from "@solana/kit";
import { buildV1Message } from "../src/v1.js";

// The message-build contract (R-V1m). The address list of a v1 message must be
// duplicate-free (the protocol rejects any duplicate), ComputeBudget instructions are
// no-ops under v1 and are rejected loudly instead of dropped silently, and the serialized
// message stays within the 4096-byte limit.

const LIFETIME = { blockhash: "11111111111111111111111111111111", lastValidBlockHeight: 999n };

test("V1-MESSAGE-REJECTS-COMPUTE-BUDGET: a ComputeBudget instruction is rejected by name", () => {
  const feePayer = Keypair.generate().publicKey;
  const ixs = [ComputeBudgetProgram.setComputeUnitLimit({ units: 100_000 })];
  assert.throws(() => buildV1Message({ instructions: ixs, feePayer, lifetime: LIFETIME }), /ComputeBudget/);
});

test("V1-MESSAGE-DEDUPLICATES-ADDRESSES: re-referenced accounts compile to a duplicate-free address list", () => {
  const feePayer = Keypair.generate().publicKey;
  const a = Keypair.generate().publicKey;
  const b = Keypair.generate().publicKey;
  const ixs = [
    SystemProgram.transfer({ fromPubkey: a, toPubkey: b, lamports: 1n }),
    SystemProgram.transfer({ fromPubkey: a, toPubkey: b, lamports: 1n }),
  ];

  let message: ReturnType<typeof buildV1Message> | undefined;
  assert.doesNotThrow(() => {
    message = buildV1Message({ instructions: ixs, feePayer, lifetime: LIFETIME });
  });

  const compiled = compileTransactionMessage(message!);
  const addresses = compiled.staticAccounts as readonly string[];
  assert.equal(new Set(addresses).size, addresses.length, "static address list must be duplicate-free");
  assert.ok(addresses.length >= 3, "a, b and the fee payer must all be present");
});

test("V1-MESSAGE-REJECTS-OVERSIZED: a message that would serialize past 4096 bytes is rejected", () => {
  const feePayer = Keypair.generate().publicKey;
  const huge = new TransactionInstruction({
    programId: Keypair.generate().publicKey,
    keys: Array.from({ length: 3 }, () => ({
      pubkey: Keypair.generate().publicKey,
      isSigner: false,
      isWritable: false,
    })),
    data: Buffer.alloc(4200, 7),
  });
  assert.throws(() => buildV1Message({ instructions: [huge], feePayer, lifetime: LIFETIME }), /4096/);
});
