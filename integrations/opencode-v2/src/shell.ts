import { normalizeTerminalIdentity } from "../../runtime/src/awareness.js";

export type ShellRedirect =
  | { readonly kind: "passthrough" }
  | { readonly kind: "run"; readonly command: string }
  | { readonly kind: "refuse"; readonly reason: string };

/** Decide what OpenCode's built-in shell should run. */
export function redirectShell(input: {
  readonly command: string;
  readonly parent: string | null;
  readonly siblings: readonly string[];
  readonly executable: string;
  readonly socket?: string;
  readonly timeoutMs: number;
}): ShellRedirect {
  const siblings = uniqueSiblings(input.siblings, input.parent);
  if (siblings.length > 1) {
    return {
      kind: "refuse",
      reason: "Several sibling panes are selected. Use phux_run with an exact target.",
    };
  }
  const sibling = siblings[0];
  if (sibling !== undefined) {
    return {
      kind: "run",
      command: phuxRunCommand(
        input.executable,
        input.socket,
        sibling,
        input.command,
        timeoutSeconds(input.timeoutMs),
      ),
    };
  }
  if (input.parent !== null) {
    return {
      kind: "refuse",
      reason:
        "Refusing OpenCode's shell: the only pane is this agent. Create a sibling with phux_create, select it, then retry.",
    };
  }
  return { kind: "passthrough" };
}

function uniqueSiblings(targets: readonly string[], parent: string | null): string[] {
  const found = new Set<string>();
  for (const target of targets) {
    const normalized = normalizeTerminalIdentity(target);
    if (normalized === null || normalized === parent) continue;
    found.add(normalized);
  }
  return [...found];
}

function timeoutSeconds(timeoutMs: number): number {
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) return 30;
  return Math.min(86_400, Math.max(1, Math.ceil(timeoutMs / 1000)));
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

function phuxRunCommand(
  executable: string,
  socket: string | undefined,
  target: string,
  command: string,
  seconds: number,
): string {
  const parts = [shellQuote(executable), "run", "--json", "--timeout", String(seconds)];
  if (socket !== undefined && socket.length > 0) parts.push("--socket", shellQuote(socket));
  parts.push(shellQuote(target), shellQuote(command));
  return parts.join(" ");
}
