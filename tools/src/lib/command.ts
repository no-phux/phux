import { status } from "./output.ts";

// The command contract main.ts runs. A command parses its own arguments and
// throws; main.ts turns the throw into the exit code and message, so every
// command shares one usage, --help and error convention.

/** One `bun tools/main.ts <name>` subcommand. Returns the process exit code. */
export interface Command {
  readonly summary: string;
  /** The argument synopsis after the command name; omitted when it takes none. */
  readonly usage?: string;
  /** More --help text, after the usage line. */
  readonly help?: string;
  run(args: string[]): number | Promise<number>;
}

/** A bad command line: main.ts prints the command's usage and exits 2. */
export class UsageError extends Error {}

/** An expected failure: main.ts prints the message, without a stack, and exits 1. */
export class Failure extends Error {}

/** A `node:util` parseArgs rejection (unknown flag, missing value, ...). */
export function isParseArgsError(error: unknown): error is Error {
  return (
    error instanceof Error &&
    "code" in error &&
    typeof error.code === "string" &&
    error.code.startsWith("ERR_PARSE_ARGS_")
  );
}

/** An error's message, or the thrown value as text. */
export function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** A flag or positional the command cannot run without. */
export function required<T>(value: T | undefined, name: string): T {
  if (value === undefined) throw new UsageError(`${name} is required`);
  return value;
}

export function usageLine(name: string, command: Command): string {
  return `usage: bun tools/main.ts ${name}${command.usage === undefined ? "" : ` ${command.usage}`}`;
}

/**
 * Run one command with the shared conventions: a leading --help or -h prints
 * its usage; a command without `usage` refuses arguments; UsageError and
 * parseArgs rejections exit 2 with the usage; Failure exits 1 with its message.
 */
export async function invoke(name: string, command: Command, args: string[]): Promise<number> {
  if (args[0] === "--help" || args[0] === "-h") {
    console.log(
      [usageLine(name, command), "", command.summary, command.help]
        .filter((part) => part !== undefined)
        .join("\n"),
    );
    return 0;
  }
  try {
    if (command.usage === undefined && args.length > 0) {
      throw new UsageError(`takes no arguments, got ${args.join(" ")}`);
    }
    return await command.run(args);
  } catch (error) {
    if (error instanceof UsageError || isParseArgsError(error)) {
      console.error(`${usageLine(name, command)}\n${name}: error: ${error.message}`);
      return 2;
    }
    if (error instanceof Failure) {
      status("error", error.message);
      return 1;
    }
    throw error;
  }
}
