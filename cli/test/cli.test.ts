import { test } from "node:test";
import assert from "node:assert/strict";
import { runCli } from "../src/index.js";
import { tokenize } from "../src/args.js";
import { parseSide, parseUsize } from "../src/units.js";

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

const COMMANDS = ["open", "close", "deposit", "withdraw", "position", "funding", "mark", "index"];

test("--help prints usage and the command list", async () => {
  const { out, io } = capture();
  const code = await runCli(["--help"], io);
  assert.equal(code, 0);
  const s = out.join("\n");
  assert.match(s, /usage:/);
  for (const cmd of COMMANDS) {
    assert.match(s, new RegExp(cmd));
  }
});

test("--help with no command exits 0", async () => {
  const { io } = capture();
  assert.equal(await runCli([], io), 0);
});

test("--version prints the package version", async () => {
  const { out, io } = capture();
  assert.equal(await runCli(["--version"], io), 0);
  assert.equal(out.join("\n").trim(), "0.1.0");
});

test("<command> --help prints command usage", async () => {
  const { out, io } = capture();
  const code = await runCli(["open", "--help"], io);
  assert.equal(code, 0);
  assert.match(out.join("\n"), /open <side> <size> <price>/);
});

test("open dry-run builds the open_position instruction", async () => {
  const { out, io } = capture();
  const code = await runCli(["open", "long", "100", "100"], io);
  assert.equal(code, 0);
  const s = out.join("\n");
  assert.match(s, /open_position/);
  assert.match(s, /derived:/);
  assert.match(s, /notional/);
  assert.match(s, /mode: offline/);
});

test("open accepts -h and reports an invalid side with non-zero exit", async () => {
  const { err, io } = capture();
  const code = await runCli(["open", "bogus", "100", "100"], io);
  assert.notEqual(code, 0);
  assert.match(err.join("\n"), /invalid side/);
});

test("close dry-run builds close_position and can chain settle_close", async () => {
  const { out, io } = capture();
  const code = await runCli(["close", "short", "50", "--settle"], io);
  assert.equal(code, 0);
  const s = out.join("\n");
  assert.match(s, /close_position/);
  assert.match(s, /settle_close/);
});

test("deposit dry-run derives the owner's associated token account", async () => {
  const { out, io } = capture();
  const code = await runCli(
    ["deposit", "100", "--mint", "11111111111111111111111111111111"],
    io,
  );
  assert.equal(code, 0);
  assert.match(out.join("\n"), /deposit_collateral/);
  assert.match(out.join("\n"), /user_ata/);
});

test("unknown command errors with a non-zero exit", async () => {
  const { err, io } = capture();
  const code = await runCli(["bogus"], io);
  assert.notEqual(code, 0);
  assert.match(err.join("\n"), /unknown command/);
});

test("tokenize parses positionals, value flags, bool flags, and = forms", () => {
  const p = tokenize([
    "open",
    "long",
    "100",
    "--price",
    "200",
    "--submit",
    "--rpc-url=https://x",
    "--",
    "extra",
  ]);
  assert.deepEqual(p.positionals, ["open", "long", "100", "extra"]);
  assert.equal(p.flags["price"], "200");
  assert.equal(p.flags["submit"], true);
  assert.equal(p.flags["rpc-url"], "https://x");
});

test("parseUsize scales decimals and rejects negative / non-numeric", () => {
  assert.equal(parseUsize("1.5", "x", 1_000_000n), 1_500_000n);
  assert.equal(parseUsize("1000", "x"), 1000n);
  assert.equal(parseUsize("2.25", "x", 1_000_000n), 2_250_000n);
  assert.throws(() => parseUsize("-5", "x"));
  assert.throws(() => parseUsize("abc", "x"));
});

test("parseSide maps human tokens to the on-chain side byte", () => {
  assert.equal(parseSide("long"), 0);
  assert.equal(parseSide("bid"), 0);
  assert.equal(parseSide("short"), 1);
  assert.equal(parseSide("ask"), 1);
  assert.throws(() => parseSide("wat"));
});
