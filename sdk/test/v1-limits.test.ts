import { test } from "node:test";
import assert from "node:assert/strict";
import { limitsFromSimulation, roundUpToPage } from "../src/v1.js";

// v1 resource limits are explicit and page-aligned (R-V1x). Unset v1 limits budget ZERO,
// so the estimation path must always produce concrete numbers; the loaded-accounts data
// size is rounded UP to the next 32 KiB page.

test("V1-PAGE-ROUNDING: roundUpToPage returns the next multiple of the 32 KiB page for any byte count", () => {
  assert.equal(roundUpToPage(0), 0);
  assert.equal(roundUpToPage(1), 32768);
  assert.equal(roundUpToPage(32767), 32768);
  assert.equal(roundUpToPage(32768), 32768);
  assert.equal(roundUpToPage(32769), 65536);
  assert.equal(roundUpToPage(100_000), 131072);
});

test("V1-LIMITS-FROM-SIMULATION: limits derived from one simulation are explicit and page-aligned", () => {
  const limits = limitsFromSimulation({ unitsConsumed: 55_000, loadedAccountsDataSize: 1_000 });
  assert.equal(limits.computeUnitLimit, 55_000);
  assert.equal(limits.loadedAccountsDataSizeLimit, 32768);

  // A data size already on a page boundary stays there; the CU limit is the raw measurement.
  const exact = limitsFromSimulation({ unitsConsumed: 1, loadedAccountsDataSize: 32768 });
  assert.equal(exact.computeUnitLimit, 1);
  assert.equal(exact.loadedAccountsDataSizeLimit, 32768);

  // A data size just past a boundary moves to the next page.
  const past = limitsFromSimulation({ unitsConsumed: 200_000, loadedAccountsDataSize: 32769 });
  assert.equal(past.loadedAccountsDataSizeLimit, 65536);
});
