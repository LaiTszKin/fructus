//! Fructus trader CLI entry point (issue #11).
//!
//! Each command builds the corresponding SDK instruction and prints a dry-run
//! report by default; `--network` connects for live queries and `--submit` signs
//! + submits on-chain. No secrets are committed.

import { flagBool } from "./args.js";
import { tokenize } from "./args.js";
import { commands, commandNames } from "./commands/registry.js";
import { submitReport } from "./commands/common.js";
import { resolveConfig } from "./env.js";
import { CliError, die } from "./errors.js";
import { defaultIo, renderReport, type Io } from "./report.js";

export const VERSION = "0.1.0";

function globalHelp(): string {
  const lines: string[] = [
    "fructus-cli — Fructus Solana yield-perp trader CLI",
    "",
    "usage: fructus-cli <command> [args] [options]",
    "",
    "commands:",
  ];
  for (const name of commandNames()) {
    const first = commands[name].usage.split("\n")[0];
    lines.push(`  ${name.padEnd(10)} ${first}`);
  }
  lines.push(
    "",
    "global options:",
    "  --rpc-url <url>      RPC endpoint (env RPC_URL)",
    "  --program-id <pk>    program id (env PROGRAM_ID)",
    "  --market <pk>        market address (env MARKET_ADDRESS)",
    "  --index-source <pk>  index/stake-pool source (env INDEX_SOURCE)",
    "  --mint <pk>          collateral mint (env COLLATERAL_MINT)",
    "  --keypair <json>     trader keypair JSON (env TRADER_KEYPAIR)",
    "  --keypair-file <p>   trader keypair file",
    "  --network/-n         connect to the RPC for live queries",
    "  --submit/-s          sign + submit on-chain",
    "  --help/-h            show help",
    "  --version/-v         print version",
    "",
    "run 'fructus-cli <cmd> --help' for command-specific usage",
  );
  return lines.join("\n");
}

/**
 * Run the CLI against `argv` (already stripped of node/script). Returns the exit
 * code. I/O is injected so tests can capture output.
 */
export async function runCli(
  argv: string[],
  io: Io = defaultIo,
  cwd = process.cwd(),
): Promise<number> {
  const parsed = tokenize(argv);
  const showHelp = flagBool(parsed.flags, "help");
  const showVersion = flagBool(parsed.flags, "version");

  if (showVersion) {
    io.out(VERSION);
    return 0;
  }

  if (parsed.positionals.length === 0) {
    io.out(globalHelp());
    return 0;
  }

  const cmdName = parsed.positionals[0];
  const cmd = commands[cmdName];
  if (!cmd) {
    io.err(`unknown command: ${cmdName}`);
    io.err(globalHelp());
    return 2;
  }

  if (showHelp) {
    io.out(`fructus-cli ${cmdName}\n\n${cmd.usage}`);
    return 0;
  }

  try {
    const cfg = resolveConfig(parsed, cwd);
    const report = await cmd.build(cfg, {
      ...parsed,
      positionals: parsed.positionals.slice(1),
    });
    io.out(renderReport(report));

    if (cfg.submit) {
      if (report.instructions.length === 0) {
        io.err(`warning: ${cmdName} has no on-chain instruction to submit`);
      } else {
        const sigs = await submitReport(cfg, report);
        io.out(`submitted: ${sigs.join(", ")}`);
      }
    }
    return 0;
  } catch (err) {
    if (err instanceof CliError) {
      io.err(`error: ${err.message}`);
      return err.exitCode;
    }
    io.err(`error: ${(err as Error).message}`);
    return 1;
  }
}

/** Default entry: parse `process.argv`, run, exit with the returned code. */
export async function main(): Promise<void> {
  const code = await runCli(process.argv.slice(2));
  if (code !== 0) {
    process.exitCode = code;
  }
}

// Invoked as a script (not when imported by tests).
const isMain = process.argv[1]?.endsWith("src/index.ts") || process.argv[1]?.endsWith("dist/src/index.js");
if (isMain) {
  main().catch((err) => {
    // eslint-disable-next-line no-console
    console.error(err);
    process.exit(1);
  });
}
