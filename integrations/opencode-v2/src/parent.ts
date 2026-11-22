import { normalizeTerminalIdentity } from "../../runtime/src/awareness.js";

/** Stable instruction. It does not name a pane, so it stays cacheable. */
export const PARENT_PANE_RULE =
  "Run commands with the built-in shell tool, including builds and tests; use its background option for dev servers and watchers. Use the phux tools only when a terminal itself is the point: an interactive program (REPL, TUI, debugger) you must drive, a long-running process the user should watch in a pane, or another agent's pane you observe or prompt. Never spawn a terminal just to run a command. Reuse a terminal you already spawned, and close it with phux_kill when done; this session's terminals are closed when it is deleted. This OpenCode process may itself run in a phux pane; phux refuses input into that pane.";

/** Canonical pane id, or null when OpenCode is not inside a phux pane. */
export function parentPane(value: string | undefined): string | null {
  return normalizeTerminalIdentity(value);
}
