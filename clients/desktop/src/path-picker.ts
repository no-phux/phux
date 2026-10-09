/** PATH_QUERY results are server-host paths, never paths on the Desktop machine. */
export interface PathRow {
  path: string;
  kind: string;
}

/** A host directory the path picker can start a terminal in. Files insert only. */
export function directoryForTerminal(kind: string, path: string): string | undefined {
  if (kind !== "directory") return undefined;
  const directory = path.trim();
  return directory === "" ? undefined : directory;
}

export interface PathResult {
  requestId: number;
  root: string;
  parent?: string;
  rows?: PathRow[];
  status?: string;
  error?: string;
  message?: string;
}

export interface PathTarget {
  terminalId: string;
  placementId: string;
  serverId: string;
  epoch: string;
}

export interface PickerState {
  target: PathTarget;
  root: string;
  query: string;
  rows: PathRow[];
  parent?: string | undefined;
  pending?: number | undefined;
  message: string;
}

export function beginPicker(target: PathTarget, root: string): PickerState {
  return { target, root, query: "", rows: [], message: "" };
}

export function canOfferPathPicker(
  features: readonly string[] | undefined,
  attached: boolean,
  target: PathTarget | undefined,
): boolean {
  return attached && target !== undefined && features?.includes("path-query") === true;
}

export function startQuery(state: PickerState, requestId: number, query: string): PickerState {
  return { ...state, query, pending: requestId, rows: [], message: "Searching host paths…" };
}

export function acceptResult(
  state: PickerState | undefined,
  reply: PathResult,
): PickerState | undefined {
  if (!state || state.pending !== reply.requestId) return state;
  if (reply.error) {
    return { ...state, pending: undefined, rows: [], message: reply.message || reply.error };
  }
  return {
    ...state,
    pending: undefined,
    root: reply.root,
    parent: reply.parent,
    rows: reply.rows ?? [],
    message:
      reply.status === "truncated"
        ? "Results truncated; narrow the search."
        : reply.status === "warming"
          ? "Host index is warming; search again shortly."
          : "",
  };
}

export function sameTarget(target: PathTarget, current: PathTarget | undefined): boolean {
  return (
    current?.terminalId === target.terminalId &&
    current.placementId === target.placementId &&
    current.serverId === target.serverId &&
    current.epoch === target.epoch
  );
}

/** Admission is checked again at paste time; a search reply alone grants nothing. */
export function preparedInsertion(
  state: PickerState | undefined,
  path: string,
  current: PathTarget | undefined,
  ready: boolean,
): string | undefined {
  if (!state || state.pending !== undefined || !ready || !sameTarget(state.target, current)) {
    return undefined;
  }
  if (!state.rows.some((row) => row.path === path)) return undefined;
  return shellQuotePath(path);
}

/** A single POSIX-shell word, not a command or a line ending in Enter. */
export function shellQuotePath(path: string): string | undefined {
  if (!path.startsWith("/")) return undefined;
  for (const character of path) {
    if (isTerminalControl(character)) return undefined;
  }
  return `'${path.replaceAll("'", `'"'"'`)}'`;
}

/** Host text as the picker shows it: controls escaped, never rendered raw (L3 section 5). */
export function displayPath(path: string): string {
  let shown = "";
  for (const character of path) {
    shown += isTerminalControl(character)
      ? `\\u{${(character.codePointAt(0) ?? 0).toString(16)}}`
      : character;
  }
  return shown;
}

function isTerminalControl(character: string): boolean {
  const code = character.codePointAt(0) ?? 0;
  return code < 0x20 || (code >= 0x7f && code <= 0x9f);
}

/** Resource IDs are the shared FFI's local:<id> or satellite:<host>:<id> spelling. */
export function hostForTerminal(terminalId: string): string | undefined {
  if (terminalId.startsWith("local:")) return undefined;
  const raw = terminalId.startsWith("satellite:") ? terminalId.slice("satellite:".length) : "";
  const split = raw.lastIndexOf(":");
  return split > 0 && /^\d+$/u.test(raw.slice(split + 1)) ? raw.slice(0, split) : undefined;
}
