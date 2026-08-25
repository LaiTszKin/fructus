//! Minimal argv tokenizer + flag parser, dependency-free (R-C1 / R-C3 smoke
//! tests exercise plain arg-parsing without a CLI framework).

/** The result of tokenizing `process.argv`. */
export interface ParsedArgs {
  /** Non-flag positional arguments, in order. */
  positionals: string[];
  /** Flags keyed by their long/short name. `--x` ⇒ `"x"`, `-x` ⇒ `"x"`. */
  flags: Record<string, string | boolean>;
}

const BOOL_FLAGS = new Set([
  "help",
  "h",
  "network",
  "n",
  "submit",
  "s",
  "version",
  "v",
  "raw",
]);

function isValue(next: string | undefined): boolean {
  return next !== undefined && !next.startsWith("-");
}

/**
 * Tokenize an argv slice into positionals + flags.
 *
 * Supports `--flag value`, `--flag=value`, `--flag` (boolean), `-x value`, and
 * the single-char boolean aliases `-h/-n/-s/-v`. `--` terminates flag parsing.
 */
export function tokenize(argv: string[]): ParsedArgs {
  const positionals: string[] = [];
  const flags: Record<string, string | boolean> = {};

  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--") {
      positionals.push(...argv.slice(i + 1));
      break;
    }
    if (a.startsWith("--")) {
      const eq = a.indexOf("=");
      if (eq !== -1) {
        flags[a.slice(2, eq)] = a.slice(eq + 1);
        continue;
      }
      const key = a.slice(2);
      if (isValue(argv[i + 1]) && !BOOL_FLAGS.has(key)) {
        flags[key] = argv[i + 1];
        i++;
      } else {
        flags[key] = true;
      }
    } else if (a.startsWith("-") && a.length > 1) {
      const key = a.slice(1);
      if (isValue(argv[i + 1]) && !BOOL_FLAGS.has(key)) {
        flags[key] = argv[i + 1];
        i++;
      } else {
        flags[key] = true;
      }
    } else {
      positionals.push(a);
    }
  }
  return { positionals, flags };
}

/** Read a string flag, falling back to `dflt`. */
export function flagStr(
  flags: Record<string, string | boolean>,
  key: string,
  dflt?: string,
): string | undefined {
  const v = flags[key];
  if (typeof v === "string") {
    return v;
  }
  return dflt;
}

/** Read the first string flag among `keys` (returns `undefined` if none). */
export function flagStrAny(
  flags: Record<string, string | boolean>,
  keys: string[],
): string | undefined {
  for (const k of keys) {
    const v = flags[k];
    if (typeof v === "string") {
      return v;
    }
  }
  return undefined;
}

/** Read a boolean flag. */
export function flagBool(
  flags: Record<string, string | boolean>,
  key: string,
): boolean {
  const v = flags[key];
  return typeof v === "string" ? v !== "false" : v === true;
}
