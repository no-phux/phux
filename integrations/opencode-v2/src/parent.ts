import { normalizeTerminalIdentity } from "../../runtime/src/awareness.js";

/** Stable instruction. It does not name a pane, so it stays cacheable. */
export const PARENT_PANE_RULE =
  "phux owns the terminals. This OpenCode process may be running inside a phux pane. Do not type into that pane: phux_run, phux_send_keys, phux_paste, and phux_agent_prompt refuse it. Create a sibling with phux_create or phux_spawn, then run shell work there. The built-in shell runs on that selected sibling and refuses when the only pane is this agent. Use phux_agent_prompt only for agent panes, not phux_run. Do not use OpenCode's built-in terminal or PTY for that work.";

/** Canonical pane id, or null when OpenCode is not inside a phux pane. */
export function parentPane(value: string | undefined): string | null {
  return normalizeTerminalIdentity(value);
}
