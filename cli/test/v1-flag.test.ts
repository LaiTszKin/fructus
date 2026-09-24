import { test } from "node:test";
import assert from "node:assert/strict";
import { runCli } from "../src/index.js";

// CLI opt-in contract (R-V1c): `--tx-version <legacy|v1>` with `legacy` as the default.
// Every dry-run report names the transaction version in effect; an unknown value is
// rejected with an error that names the flag and the legal values.

/** A capturing Io so tests can assert on CLI output without a real terminal. */
function capture() {
  const out: string[] = [];
  const err: string[] = [];
  return {
    out,
    err,
    io: { out: (s: string) => out.push(s), err: (s: string) => err.push(s) },
  };
}

test("CLI-TX-VERSION-DEFAULT: the dry-run report names the legacy transaction version by default", async () => {
  const { out, io } = capture();
  const code = await runCli(["open", "long", "100", "200"], io);
  assert.equal(code, 0);
  const s = out.join("\n");
  assert.match(s, /tx-version: legacy/);
  assert.doesNotMatch(s, /tx-version: v1/);
});

test("CLI-TX-VERSION-V1: --tx-version v1 is honoured in the dry-run report", async () => {
  const { out, io } = capture();
  const code = await runCli(["open", "long", "100", "200", "--tx-version", "v1"], io);
  assert.equal(code, 0);
  assert.match(out.join("\n"), /tx-version: v1/);
});

test("CLI-TX-VERSION-INVALID: an unknown --tx-version value is rejected with an error naming the flag and its legal values", async () => {
  const { err, io } = capture();
  const code = await runCli(["open", "long", "100", "200", "--tx-version", "banana"], io);
  assert.notEqual(code, 0);
  const e = err.join("\n");
  assert.match(e, /tx-version/);
  assert.match(e, /legacy/);
  assert.match(e, /v1/);
});
