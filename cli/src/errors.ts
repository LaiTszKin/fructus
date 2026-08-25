//! Error type + exit-code mapping for the Fructus CLI.

/** A user-facing CLI error with an associated exit code. */
export class CliError extends Error {
  readonly exitCode: number;
  constructor(message: string, exitCode = 1) {
    super(message);
    this.name = "CliError";
    this.exitCode = exitCode;
  }
}

/** Throw a `CliError` with `msg`, exiting with `code`. */
export function die(msg: string, code = 1): never {
  throw new CliError(msg, code);
}
