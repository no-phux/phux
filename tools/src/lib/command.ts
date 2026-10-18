/** One `bun tools/main.ts <name>` subcommand. Returns the process exit code. */
export interface Command {
  readonly summary: string;
  run(args: string[]): number | Promise<number>;
}
