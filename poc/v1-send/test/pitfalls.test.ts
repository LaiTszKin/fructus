import { test } from "node:test";
import assert from "node:assert/strict";
import { renderPitfalls } from "../main.js";

// Regression: the first live devnet run sent and confirmed the v1 transfer, then
// crashed in its own step-6 reporting — the pitfalls rows passed kit's message config
// straight to `JSON.stringify`, and a v1 config carries `u64` (bigint) entries:
//   TypeError: Do not know how to serialize a BigInt
// The rows now go through `bigintReplacer`; these tests call the renderer with the
// exact shapes the walk produces (provisory = two u32 zeros; configured = u64 + u32 +
// u32) and with adversarial extras, so a future call site that forgets the replacer
// fails here instead of after a successful (paid) send.

test("PITFALLS-RENDER-BIGINT-SAFE: rendering the rows never throws on kit's bigint config values", () => {
  const rows = renderPitfalls({
    provisory: {
      configValues: [
        { kind: "u32", value: 0 },
        // adversarial: a v1 config may carry u64 entries on either call site
        { kind: "u64", value: 0n },
      ],
      numStaticAccounts: 2,
    },
    measuredConfigValues: [
      { kind: "u64", value: 0n },
      { kind: "u32", value: 150 },
      { kind: "u32", value: 32768 },
    ],
    wireBytes: 204,
  });

  assert.ok(rows.length >= 6, "every pitfall gets a row");
  for (const [head, detail] of rows) {
    assert.equal(typeof head, "string");
    assert.equal(typeof detail, "string");
    assert.ok(head.length > 0 && detail.length > 0);
  }

  const text = rows.map(([head, detail]) => `${head}\n${detail}`).join("\n");
  assert.match(text, /0n/, "the u64 entry survives as a readable 0n");
  assert.match(text, /32768/, "the page-rounded data size is rendered");
  assert.match(text, /duplicate addresses are rejected/, "the probe-B row survives extraction");
});

test("PITFALLS-RENDER-BUILD-ONLY: the build-only path renders without measured values", () => {
  const rows = renderPitfalls({
    provisory: { configValues: [{ kind: "u32", value: 0 }], numStaticAccounts: 2 },
    wireBytes: 198,
  });
  const text = rows.map(([head, detail]) => `${head}\n${detail}`).join("\n");
  assert.doesNotMatch(text, /, measured /, "no measured clause when nothing was configured");
  assert.match(text, /198 bytes/);
});
