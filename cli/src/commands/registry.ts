//! Command registry (R-C1).

import type { ParsedArgs } from "../args.js";
import type { TraderConfig } from "../env.js";
import type { DryRunReport } from "../report.js";
import * as close from "./close.js";
import * as deposit from "./deposit.js";
import * as funding from "./funding.js";
import * as indexCmd from "./index.js";
import * as mark from "./mark.js";
import * as open from "./open.js";
import * as position from "./position.js";
import * as withdraw from "./withdraw.js";

export interface Command {
  usage: string;
  build: (cfg: TraderConfig, args: ParsedArgs) => Promise<DryRunReport>;
}

export const commands: Record<string, Command> = {
  open,
  close,
  deposit,
  withdraw,
  position,
  funding,
  mark,
  index: indexCmd,
};

export function commandNames(): string[] {
  return Object.keys(commands);
}
