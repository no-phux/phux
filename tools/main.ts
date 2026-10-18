#!/usr/bin/env bun
// Build and CI logic for phux: `bun tools/main.ts <command> [args]`.
// Each area owns its commands; this file only lists them.
import { invoke, type Command } from "./src/lib/command.ts";
import { commands as cargo } from "./src/cargo/commands.ts";
import { commands as desktop } from "./src/desktop/commands.ts";
import { commands as font } from "./src/font/commands.ts";
import { commands as integrations } from "./src/integrations/commands.ts";
import { commands as mbx } from "./src/mbx/commands.ts";
import { commands as mutation } from "./src/mutation/commands.ts";
import { commands as perf } from "./src/perf/commands.ts";
import { commands as release } from "./src/release/commands.ts";
import { commands as routing } from "./src/routing/commands.ts";
import { commands as web } from "./src/web/commands.ts";

const COMMANDS: Record<string, Command> = {
  ...cargo,
  ...desktop,
  ...font,
  ...integrations,
  ...mbx,
  ...mutation,
  ...perf,
  ...release,
  ...routing,
  ...web,
};

function usage(): string {
  const width = Math.max(...Object.keys(COMMANDS).map((name) => name.length));
  const lines = Object.entries(COMMANDS)
    .toSorted(([left], [right]) => left.localeCompare(right))
    .map(([name, command]) => `  ${name.padEnd(width)}  ${command.summary}`);
  return ["usage: bun tools/main.ts <command> [args]", "", "commands:", ...lines].join("\n");
}

const [name, ...args] = process.argv.slice(2);
const command = name === undefined ? undefined : COMMANDS[name];
if (name === undefined || command === undefined) {
  const help = name === undefined || name === "--help" || name === "-h";
  (help ? console.log : console.error)(usage());
  process.exit(help ? 0 : 2);
}
process.exit(await invoke(name, command, args));
