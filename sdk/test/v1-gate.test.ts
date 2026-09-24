import { test } from "node:test";
import assert from "node:assert/strict";
import type { Connection } from "@solana/web3.js";
import { TXV1_GATE_ID, V1UnavailableError, isV1GateActive, sendV1Instructions } from "../src/v1.js";

// The gate contract (R-V1g): the feature account's presence IS the activation signal;
// every v1 send checks it FIRST and fails closed with an error that names the gate —
// nothing is built, nothing is sent.

/** A minimal fake Connection: only `getAccountInfo` is used by the gate check. */
function fakeConnection(featureAccountExists: boolean): Connection {
  return {
    getAccountInfo: async () => (featureAccountExists ? { data: new Uint8Array(9) } : null),
  } as unknown as Connection;
}

test("V1-GATE-ACTIVE: the gate reads active exactly when the feature account exists", async () => {
  assert.equal(await isV1GateActive(fakeConnection(true)), true);
});

test("V1-GATE-INACTIVE-FAILS-CLOSED: an inactive gate reads false and any v1 send rejects naming the gate", async () => {
  const off = fakeConnection(false);
  assert.equal(await isV1GateActive(off), false);

  await assert.rejects(
    sendV1Instructions(off, [], []),
    (err: unknown) =>
      err instanceof V1UnavailableError &&
      (err as Error).message.includes("txv1aq4pp281K9um3pgkfX8UqtFT6wcVW3hNezGLL"),
  );
});

test("V1-GATE-ID: the gate id constant is the documented feature id", () => {
  assert.equal(TXV1_GATE_ID, "txv1aq4pp281K9um3pgkfX8UqtFT6wcVW3hNezGLL");
});
