export interface SessionSummary {
  readonly name: string;
  readonly windows: number;
  readonly attached: boolean;
}

export interface SessionList {
  readonly schema_version: 1 | 2;
  readonly sessions: readonly SessionSummary[];
  /** Canonical selectors for every addressable terminal (v2; empty for v1). */
  readonly terminals: readonly string[];
}

export interface CursorState {
  readonly x: number;
  readonly y: number;
  readonly visible: boolean;
}

export type CellColor =
  | { readonly kind: "default" }
  | { readonly kind: "palette"; readonly index: number }
  | { readonly kind: "rgb"; readonly r: number; readonly g: number; readonly b: number };

export interface CellStyle {
  readonly bold: boolean;
  readonly faint: boolean;
  readonly italic: boolean;
  readonly underline: boolean;
  readonly blink: boolean;
  readonly inverse: boolean;
  readonly invisible: boolean;
  readonly strikethrough: boolean;
  readonly overline: boolean;
  readonly fg: CellColor;
  readonly bg: CellColor;
}

export interface CellInfo {
  readonly col: number;
  readonly row: number;
  readonly semantic?: "output" | "input" | "prompt";
  readonly style: CellStyle;
}

export interface ScreenState {
  readonly schema_version: 1 | 2 | 3;
  readonly pane: number;
  readonly cols: number;
  readonly rows: number;
  readonly cursor: CursorState | null;
  readonly lines: readonly string[];
  readonly scrollback: readonly string[];
  readonly cells?: readonly CellInfo[];
}

export interface RunResult {
  readonly command: string;
  readonly exit_code: number;
  readonly output: string;
  readonly duration_ms: number;
  readonly truncated: boolean;
}

export interface CreateResult {
  readonly session: string;
  readonly terminal_id: number;
}

export interface SpawnResult {
  readonly terminal_id: number;
  readonly satellite: string | null;
}

export interface LaunchResult {
  readonly schema_version: 1;
  readonly terminal_id: number;
  readonly integration: string;
  readonly plugin: string;
  /** Validated because it is part of the CLI response, but never rendered to the model. */
  readonly argv: readonly string[];
}

export type SpatialDirection = "horizontal" | "vertical";

export interface InsertPaneResult {
  readonly schema_version: 1;
  readonly operation: "insert-pane";
  readonly session_id: number;
  readonly target_terminal_id: number;
  readonly new_terminal_id: number;
  readonly direction: SpatialDirection;
  readonly ratio: number;
}

export interface MovePaneResult {
  readonly schema_version: 1;
  readonly operation: "move-pane";
  readonly session_id: number;
  readonly source_terminal_id: number;
  readonly target_terminal_id: number;
  readonly direction: SpatialDirection;
  readonly ratio: number;
}

export interface SwapPaneResult {
  readonly schema_version: 1;
  readonly operation: "swap-pane";
  readonly session_id: number;
  readonly first_terminal_id: number;
  readonly second_terminal_id: number;
}

export interface AskedEvent {
  readonly event: "asked";
  readonly terminal: string;
  readonly id: string;
  readonly question: string;
  readonly suggestions: readonly string[];
  readonly elapsed_seconds: number | null;
}

export type WatchEvent =
  | { readonly event: "title_changed"; readonly terminal?: string; readonly title: string }
  | { readonly event: "command_started" | "bell" | "pane_spawned" | "dirty" | "idle"; readonly terminal?: string }
  | { readonly event: "command_finished"; readonly terminal?: string; readonly exit_code: number | null }
  | { readonly event: "pane_closed"; readonly terminal?: string; readonly exit_status: number | null }
  | ({ readonly event: "asked"; readonly terminal?: string } & Omit<AskedEvent, "event" | "terminal">)
  | { readonly event: "unknown"; readonly terminal?: string; readonly tag: number };

export interface RenderedCell {
  readonly grapheme: string;
  readonly style: CellStyle;
}

export interface RenderedFrame {
  readonly schema_version: 1;
  readonly cols: number;
  readonly rows: number;
  readonly cursor: CursorState | null;
  readonly cells: readonly RenderedCell[];
}

export interface TagRow {
  readonly terminal: string;
  /** Opaque human confirmation text; the current CLI has no tag JSON shape. */
  readonly tagsText: string;
}

export type AgentKind = "codex" | "claude" | "plugin" | "declared" | "unknown";
export type AgentState = "unknown" | "idle" | "working" | "blocked" | "done";
export type AgentAttention = "none" | "low" | "normal" | "high";

export interface AgentIdentity {
  readonly id: string;
  readonly label: string;
  readonly kind: AgentKind;
}

export interface AgentSource {
  readonly kind: string;
  readonly signal: string;
  readonly confidence: number;
  readonly observed: string;
}

export interface AgentPane {
  /** Canonical phux pane selector, for example @3 or host/@3. */
  readonly terminal: string;
  readonly session: string;
  readonly window: string;
  readonly agent: AgentIdentity;
  readonly state: AgentState;
  readonly confidence: number;
  readonly attention: AgentAttention;
  readonly title: string | null;
  readonly cwd: string | null;
  readonly sources: readonly AgentSource[];
  readonly explanation: string;
}

export interface AgentStateList {
  readonly schema_version: 1;
  readonly agents: readonly AgentPane[];
}

/**
 * The declared `phux.agent/v1` record written by `phux agent set`.
 *
 * `state` and `attention` are OPTIONAL, per `docs/spec/L3.md` §3.7 where only
 * `name` is required. Declaring a `state` outranks the server's derivation for
 * the record's whole lifetime (ADR-0046 point 8), so an integration that can
 * let the server derive should omit it and write identity alone.
 */
export interface AgentRecord {
  readonly name: string;
  readonly kind: string;
  readonly state?: AgentState;
  readonly attention?: AgentAttention;
  readonly session: string;
}

export class SchemaValidationError extends Error {
  constructor(readonly path: string, expectation: string) {
    super(`${path} must be ${expectation}`);
    this.name = "SchemaValidationError";
  }
}

type RecordValue = Record<string, unknown>;

function record(value: unknown, path: string): RecordValue {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new SchemaValidationError(path, "an object");
  }
  return value as RecordValue;
}

function string(value: unknown, path: string): string {
  if (typeof value !== "string") throw new SchemaValidationError(path, "a string");
  return value;
}

function boolean(value: unknown, path: string): boolean {
  if (typeof value !== "boolean") throw new SchemaValidationError(path, "a boolean");
  return value;
}

function nullableString(value: unknown, path: string): string | null {
  return value === null ? null : string(value, path);
}

function numberInRange(value: unknown, path: string, min: number, max: number): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < min || value > max) {
    throw new SchemaValidationError(path, `a number from ${min} through ${max}`);
  }
  return value;
}

function oneOf<const T extends readonly string[]>(value: unknown, path: string, values: T): T[number] {
  if (typeof value !== "string" || !values.includes(value)) {
    throw new SchemaValidationError(path, values.map((item) => JSON.stringify(item)).join(", "));
  }
  return value as T[number];
}

function integer(value: unknown, path: string, min: number, max = Number.MAX_SAFE_INTEGER): number {
  if (!Number.isSafeInteger(value) || (value as number) < min || (value as number) > max) {
    throw new SchemaValidationError(path, `an integer from ${min} through ${max}`);
  }
  return value as number;
}

function strings(value: unknown, path: string): string[] {
  if (!Array.isArray(value)) throw new SchemaValidationError(path, "an array of strings");
  return value.map((item, index) => string(item, `${path}[${index}]`));
}

export function parseSessionList(value: unknown): SessionList {
  const root = record(value, "$ (phux ls --json CLI shape)");
  if (root.schema_version !== 1 && root.schema_version !== 2) {
    throw new SchemaValidationError("$.schema_version", "a supported value (1 or 2)");
  }
  const schema = root.schema_version;
  if (!Array.isArray(root.sessions)) {
    throw new SchemaValidationError("$.sessions", "an array");
  }
  const sessions = root.sessions.map((item, index): SessionSummary => {
    const row = record(item, `$.sessions[${index}]`);
    const name = string(row.name, `$.sessions[${index}].name`);
    if (name.length === 0) throw new SchemaValidationError(`$.sessions[${index}].name`, "non-empty");
    return {
      name,
      windows: integer(row.windows, `$.sessions[${index}].windows`, 0),
      attached: boolean(row.attached, `$.sessions[${index}].attached`),
    };
  });
  const terminals = schema === 1 && root.terminals === undefined
    ? []
    : strings(root.terminals, "$.terminals");
  return { schema_version: schema, sessions, terminals };
}

function parseColor(value: unknown, path: string): CellColor {
  const color = record(value, path);
  if (color.kind === "default") return { kind: "default" };
  if (color.kind === "palette") {
    return { kind: "palette", index: integer(color.index, `${path}.index`, 0, 255) };
  }
  if (color.kind === "rgb") {
    return {
      kind: "rgb",
      r: integer(color.r, `${path}.r`, 0, 255),
      g: integer(color.g, `${path}.g`, 0, 255),
      b: integer(color.b, `${path}.b`, 0, 255),
    };
  }
  throw new SchemaValidationError(`${path}.kind`, '"default", "palette", or "rgb"');
}

function parseStyle(value: unknown, path: string): CellStyle {
  const style = record(value, path);
  return {
    bold: boolean(style.bold, `${path}.bold`),
    faint: boolean(style.faint, `${path}.faint`),
    italic: boolean(style.italic, `${path}.italic`),
    underline: boolean(style.underline, `${path}.underline`),
    blink: boolean(style.blink, `${path}.blink`),
    inverse: boolean(style.inverse, `${path}.inverse`),
    invisible: boolean(style.invisible, `${path}.invisible`),
    strikethrough: boolean(style.strikethrough, `${path}.strikethrough`),
    overline: boolean(style.overline, `${path}.overline`),
    fg: parseColor(style.fg, `${path}.fg`),
    bg: parseColor(style.bg, `${path}.bg`),
  };
}

export function parseScreenState(value: unknown): ScreenState {
  const root = record(value, "$ (phux snapshot/wait --json CLI shape)");
  const schema = integer(root.schema_version, "$.schema_version", 1, 3) as 1 | 2 | 3;
  const cols = integer(root.cols, "$.cols", 0, 65_535);
  const rows = integer(root.rows, "$.rows", 0, 65_535);
  const lines = strings(root.lines, "$.lines");
  if (lines.length !== rows) {
    throw new SchemaValidationError("$.lines", `an array with exactly $.rows (${rows}) entries`);
  }

  let cursor: CursorState | null = null;
  if (root.cursor !== null) {
    const rawCursor = record(root.cursor, "$.cursor");
    cursor = {
      x: integer(rawCursor.x, "$.cursor.x", 0, Math.max(0, cols - 1)),
      y: integer(rawCursor.y, "$.cursor.y", 0, Math.max(0, rows - 1)),
      visible: boolean(rawCursor.visible, "$.cursor.visible"),
    };
  }

  const scrollback = root.scrollback === undefined ? [] : strings(root.scrollback, "$.scrollback");
  if (root.cells === undefined) {
    return {
      schema_version: schema,
      pane: integer(root.pane, "$.pane", 0, 4_294_967_295),
      cols,
      rows,
      cursor,
      lines,
      scrollback,
    };
  }
  if (!Array.isArray(root.cells)) throw new SchemaValidationError("$.cells", "an array");
  let previous = -1;
  const cells = root.cells.map((item, index): CellInfo => {
    const path = `$.cells[${index}]`;
    const cell = record(item, path);
    const col = integer(cell.col, `${path}.col`, 0, Math.max(0, cols - 1));
    const row = integer(cell.row, `${path}.row`, 0, Math.max(0, rows - 1));
    const position = row * cols + col;
    if (position <= previous) throw new SchemaValidationError(path, "strictly row-major with no duplicate cells");
    previous = position;
    const semantic = cell.semantic;
    if (semantic !== undefined && semantic !== "output" && semantic !== "input" && semantic !== "prompt") {
      throw new SchemaValidationError(`${path}.semantic`, '"output", "input", or "prompt"');
    }
    const result: CellInfo = { col, row, style: parseStyle(cell.style, `${path}.style`) };
    return semantic === undefined ? result : { ...result, semantic };
  });
  return {
    schema_version: schema,
    pane: integer(root.pane, "$.pane", 0, 4_294_967_295),
    cols,
    rows,
    cursor,
    lines,
    scrollback,
    cells,
  };
}

export function parseCreateResult(value: unknown): CreateResult {
  const root = record(value, "$ (phux new --json CLI shape)");
  const session = string(root.session, "$.session");
  if (session.length === 0) throw new SchemaValidationError("$.session", "non-empty");
  return {
    session,
    terminal_id: integer(root.terminal_id, "$.terminal_id", 0, 4_294_967_295),
  };
}

export function parseSpawnResult(value: unknown): SpawnResult {
  const root = record(value, "$ (phux spawn --json CLI shape)");
  const satellite = nullableString(root.satellite, "$.satellite");
  if (satellite !== null && satellite.trim().length === 0) {
    throw new SchemaValidationError("$.satellite", "null or non-empty");
  }
  return {
    terminal_id: integer(root.terminal_id, "$.terminal_id", 0, 4_294_967_295),
    satellite,
  };
}

export function parseLaunchResult(value: unknown): LaunchResult {
  const root = record(value, "$ (phux launch --json CLI shape)");
  if (root.schema_version !== 1) {
    throw new SchemaValidationError("$.schema_version", "the supported value 1");
  }
  const integration = string(root.integration, "$.integration");
  const plugin = string(root.plugin, "$.plugin");
  if (integration.trim().length === 0) throw new SchemaValidationError("$.integration", "non-empty");
  if (plugin.trim().length === 0) throw new SchemaValidationError("$.plugin", "non-empty");
  const argv = strings(root.argv, "$.argv");
  if (argv.length === 0) throw new SchemaValidationError("$.argv", "a non-empty array of strings");
  return {
    schema_version: 1,
    terminal_id: integer(root.terminal_id, "$.terminal_id", 0, 4_294_967_295),
    integration,
    plugin,
    argv,
  };
}

function spatialRoot(value: unknown, operation: string): RecordValue {
  const root = record(value, `$ (phux ${operation} --json CLI shape)`);
  if (root.schema_version !== 1) {
    throw new SchemaValidationError("$.schema_version", "the supported value 1");
  }
  if (root.operation !== operation) {
    throw new SchemaValidationError("$.operation", JSON.stringify(operation));
  }
  return root;
}

function spatialDirection(value: unknown): SpatialDirection {
  return oneOf(value, "$.direction", ["horizontal", "vertical"] as const);
}

function spatialRatio(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0 || value >= 1) {
    throw new SchemaValidationError("$.ratio", "finite and strictly between 0 and 1");
  }
  return value;
}

export function parseInsertPaneResult(value: unknown): InsertPaneResult {
  const root = spatialRoot(value, "insert-pane");
  return {
    schema_version: 1,
    operation: "insert-pane",
    session_id: integer(root.session_id, "$.session_id", 0),
    target_terminal_id: integer(root.target_terminal_id, "$.target_terminal_id", 0, 4_294_967_295),
    new_terminal_id: integer(root.new_terminal_id, "$.new_terminal_id", 0, 4_294_967_295),
    direction: spatialDirection(root.direction),
    ratio: spatialRatio(root.ratio),
  };
}

export function parseMovePaneResult(value: unknown): MovePaneResult {
  const root = spatialRoot(value, "move-pane");
  return {
    schema_version: 1,
    operation: "move-pane",
    session_id: integer(root.session_id, "$.session_id", 0),
    source_terminal_id: integer(root.source_terminal_id, "$.source_terminal_id", 0, 4_294_967_295),
    target_terminal_id: integer(root.target_terminal_id, "$.target_terminal_id", 0, 4_294_967_295),
    direction: spatialDirection(root.direction),
    ratio: spatialRatio(root.ratio),
  };
}

export function parseSwapPaneResult(value: unknown): SwapPaneResult {
  const root = spatialRoot(value, "swap-pane");
  return {
    schema_version: 1,
    operation: "swap-pane",
    session_id: integer(root.session_id, "$.session_id", 0),
    first_terminal_id: integer(root.first_terminal_id, "$.first_terminal_id", 0, 4_294_967_295),
    second_terminal_id: integer(root.second_terminal_id, "$.second_terminal_id", 0, 4_294_967_295),
  };
}

export function parseAskedEvent(value: unknown): AskedEvent {
  const root = record(value, "$ (phux ask --json CLI shape)");
  if (root.event !== "asked") throw new SchemaValidationError("$.event", '"asked"');
  const terminal = string(root.terminal, "$.terminal");
  if (!PANE_SELECTOR.test(terminal)) throw new SchemaValidationError("$.terminal", "a canonical pane selector");
  const elapsed = root.elapsed_seconds === null
    ? null
    : integer(root.elapsed_seconds, "$.elapsed_seconds", 0);
  return {
    event: "asked",
    terminal,
    id: string(root.id, "$.id"),
    question: string(root.question, "$.question"),
    suggestions: strings(root.suggestions, "$.suggestions"),
    elapsed_seconds: elapsed,
  };
}

export function parseWatchEvent(value: unknown, path = "$ (phux watch --json line)"): WatchEvent {
  const root = record(value, path);
  const event = oneOf(root.event, `${path}.event`, [
    "title_changed", "command_started", "command_finished", "bell", "pane_spawned",
    "pane_closed", "dirty", "idle", "asked", "unknown",
  ] as const);
  const terminal = root.terminal === undefined ? undefined : string(root.terminal, `${path}.terminal`);
  if (terminal !== undefined && !PANE_SELECTOR.test(terminal)) {
    throw new SchemaValidationError(`${path}.terminal`, "a canonical pane selector");
  }
  const base = terminal === undefined ? {} : { terminal };
  switch (event) {
    case "title_changed": return { event, ...base, title: string(root.title, `${path}.title`) };
    case "command_finished": return {
      event, ...base,
      exit_code: root.exit_code === null ? null : integer(root.exit_code, `${path}.exit_code`, -2_147_483_648, 2_147_483_647),
    };
    case "pane_closed": return {
      event, ...base,
      exit_status: root.exit_status === null ? null : integer(root.exit_status, `${path}.exit_status`, -2_147_483_648, 2_147_483_647),
    };
    case "asked": return {
      event, ...base,
      id: string(root.id, `${path}.id`),
      question: string(root.question, `${path}.question`),
      suggestions: strings(root.suggestions, `${path}.suggestions`),
      elapsed_seconds: root.elapsed_seconds === null ? null : integer(root.elapsed_seconds, `${path}.elapsed_seconds`, 0),
    };
    case "unknown": return { event, ...base, tag: integer(root.tag, `${path}.tag`, 0) };
    default: return { event, ...base };
  }
}

export function parseRenderedFrame(value: unknown): RenderedFrame {
  const root = record(value, "$ (phux snapshot --rendered --json CLI shape)");
  if (root.schema_version !== 1) throw new SchemaValidationError("$.schema_version", "the supported value 1");
  const cols = integer(root.cols, "$.cols", 1, 65_535);
  const rows = integer(root.rows, "$.rows", 1, 65_535);
  if (!Array.isArray(root.cells)) throw new SchemaValidationError("$.cells", "an array");
  const expected = cols * rows;
  if (root.cells.length !== expected) throw new SchemaValidationError("$.cells", `an array with exactly ${expected} entries`);
  const cells = root.cells.map((value, index): RenderedCell => {
    const path = `$.cells[${index}]`;
    const cell = record(value, path);
    return { grapheme: string(cell.grapheme, `${path}.grapheme`), style: parseStyle(cell.style, `${path}.style`) };
  });
  let cursor: CursorState | null = null;
  if (root.cursor !== null) {
    const raw = record(root.cursor, "$.cursor");
    cursor = {
      x: integer(raw.x, "$.cursor.x", 0, cols - 1),
      y: integer(raw.y, "$.cursor.y", 0, rows - 1),
      visible: boolean(raw.visible, "$.cursor.visible"),
    };
  }
  return { schema_version: 1, cols, rows, cursor, cells };
}

export function parseRunResult(value: unknown): RunResult {
  const root = record(value, "$ (phux run --json CLI shape)");
  return {
    command: string(root.command, "$.command"),
    exit_code: integer(root.exit_code, "$.exit_code", -2_147_483_648, 2_147_483_647),
    output: string(root.output, "$.output"),
    duration_ms: integer(root.duration_ms, "$.duration_ms", 0),
    truncated: boolean(root.truncated, "$.truncated"),
  };
}

const AGENT_KINDS = ["codex", "claude", "plugin", "declared", "unknown"] as const;
const AGENT_STATES = ["unknown", "idle", "working", "blocked", "done"] as const;
const AGENT_ATTENTION = ["none", "low", "normal", "high"] as const;
const PANE_SELECTOR = /^(?:[^/\s]+\/)?@\d+$/;

export function parseAgentRecord(value: unknown, path = "$ (phux.agent/v1 record)"): AgentRecord {
  const root = record(value, path);
  const name = string(root.name, `${path}.name`);
  if (name.trim().length === 0) throw new SchemaValidationError(`${path}.name`, "non-empty");
  const kind = string(root.kind, `${path}.kind`);
  if (kind.trim().length === 0) throw new SchemaValidationError(`${path}.kind`, "non-empty");
  const session = string(root.session, `${path}.session`);
  if (session.trim().length === 0) throw new SchemaValidationError(`${path}.session`, "non-empty");
  // Absent `state` / `attention` are legal (L3.md 3.7: only `name` is
  // required), so tolerate their absence rather than failing the parse. A
  // present-but-unrecognized value is still an error, as before.
  return {
    name,
    kind,
    ...(root.state === undefined
      ? {}
      : { state: oneOf(root.state, `${path}.state`, AGENT_STATES) }),
    ...(root.attention === undefined
      ? {}
      : { attention: oneOf(root.attention, `${path}.attention`, AGENT_ATTENTION) }),
    session,
  };
}

export function parseAgentStateList(value: unknown): AgentStateList {
  const root = record(value, "$ (phux agent list --json CLI shape)");
  if (root.schema_version !== 1) {
    throw new SchemaValidationError("$.schema_version", "the supported value 1");
  }
  if (!Array.isArray(root.agents)) throw new SchemaValidationError("$.agents", "an array");

  const agents = root.agents.map((item, index): AgentPane => {
    const path = `$.agents[${index}]`;
    const row = record(item, path);
    const terminal = string(row.terminal, `${path}.terminal`);
    if (!PANE_SELECTOR.test(terminal)) {
      throw new SchemaValidationError(`${path}.terminal`, "a canonical pane selector such as @3 or host/@3");
    }
    const identity = record(row.agent, `${path}.agent`);
    if (!Array.isArray(row.sources)) throw new SchemaValidationError(`${path}.sources`, "an array");
    return {
      terminal,
      session: string(row.session, `${path}.session`),
      window: string(row.window, `${path}.window`),
      agent: {
        id: string(identity.id, `${path}.agent.id`),
        label: string(identity.label, `${path}.agent.label`),
        kind: oneOf(identity.kind, `${path}.agent.kind`, AGENT_KINDS),
      },
      state: oneOf(row.state, `${path}.state`, AGENT_STATES),
      confidence: numberInRange(row.confidence, `${path}.confidence`, 0, 1),
      attention: oneOf(row.attention, `${path}.attention`, AGENT_ATTENTION),
      title: nullableString(row.title, `${path}.title`),
      cwd: nullableString(row.cwd, `${path}.cwd`),
      sources: row.sources.map((source, sourceIndex): AgentSource => {
        const sourcePath = `${path}.sources[${sourceIndex}]`;
        const raw = record(source, sourcePath);
        return {
          kind: string(raw.kind, `${sourcePath}.kind`),
          signal: string(raw.signal, `${sourcePath}.signal`),
          confidence: numberInRange(raw.confidence, `${sourcePath}.confidence`, 0, 1),
          observed: string(raw.observed, `${sourcePath}.observed`),
        };
      }),
      explanation: string(row.explanation, `${path}.explanation`),
    };
  });
  return { schema_version: 1, agents };
}
