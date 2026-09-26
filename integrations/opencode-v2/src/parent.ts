import { normalizeTerminalIdentity } from "../../runtime/src/awareness.js";

/** Stable instruction. It does not name a pane, so it stays cacheable. */
export const PARENT_PANE_RULE =
  "phux owns the terminals. This OpenCode process may be running inside a phux pane. Do not type into that pane: phux_run and phux_send_keys refuse it. Create a sibling with phux_create, then run shell work there. Do not use OpenCode's built-in terminal or PTY for that work.";

const WRITE_TOOLS = new Set(["phux_run", "phux_send_keys"]);

/** Canonical pane id, or null when OpenCode is not inside a phux pane. */
export function parentPane(value: string | undefined): string | null {
  return normalizeTerminalIdentity(value);
}

export function isWriteTool(name: string): boolean {
  return WRITE_TOOLS.has(name);
}

/**
 * True when `target` is the pane OpenCode itself is running in.
 * `host/@7` is not `@7`: a satellite selector is a different terminal.
 */
export function samePane(target: string, parent: string): boolean {
  const left = normalizeTerminalIdentity(target);
  const right = normalizeTerminalIdentity(parent);
  return left !== null && left === right;
}

export function parentWriteError(tool: string, parent: string): Error {
  return new Error(
    `Refusing ${tool} on ${parent}: that pane is running this OpenCode process. ` +
      "phux_create a sibling shell and pass that target. Do not type into the OpenCode TUI.",
  );
}
