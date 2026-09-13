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
export type CellColor = {
    readonly kind: "default";
} | {
    readonly kind: "palette";
    readonly index: number;
} | {
    readonly kind: "rgb";
    readonly r: number;
    readonly g: number;
    readonly b: number;
};
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
export type WatchEvent = {
    readonly event: "title_changed";
    readonly terminal?: string;
    readonly title: string;
} | {
    readonly event: "command_started" | "bell" | "pane_spawned" | "dirty" | "idle";
    readonly terminal?: string;
} | {
    readonly event: "command_finished";
    readonly terminal?: string;
    readonly exit_code: number | null;
} | {
    readonly event: "pane_closed";
    readonly terminal?: string;
    readonly exit_status: number | null;
} | ({
    readonly event: "asked";
    readonly terminal?: string;
} & Omit<AskedEvent, "event" | "terminal">) | {
    readonly event: "unknown";
    readonly terminal?: string;
    readonly tag: number;
};
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
/** Closed `AgentEventsJsonlV1` record types (`docs/spec/L1.md`). */
export declare const AGENT_EVENT_TYPES: readonly ["session_start", "prompt", "tool_start", "tool_end", "notification", "ask", "stop", "session_end", "state", "provider_raw"];
export type AgentEventType = (typeof AGENT_EVENT_TYPES)[number];
/** `phux agent session open --json` document. */
export interface AgentSessionOpenResult {
    readonly schema_version: 1;
    readonly resource: string;
    readonly parent: string;
    readonly provider: string;
    readonly native_id: string | null;
}
/** `phux agent emit --json` stamped header. */
export interface AgentEmitResult {
    readonly schema_version: 1;
    readonly resource: string;
    readonly seq: number;
    readonly ts_ms: number;
    readonly type: AgentEventType;
}
/** Projection of `phux agent session close`'s `@N\\tclosed` line. */
export interface AgentSessionCloseResult {
    readonly resource: string;
    readonly closed: true;
}
export declare class SchemaValidationError extends Error {
    readonly path: string;
    constructor(path: string, expectation: string);
}
export declare function parseSessionList(value: unknown): SessionList;
export declare function parseScreenState(value: unknown): ScreenState;
export declare function parseCreateResult(value: unknown): CreateResult;
export declare function parseSpawnResult(value: unknown): SpawnResult;
export declare function parseLaunchResult(value: unknown): LaunchResult;
export declare function parseInsertPaneResult(value: unknown): InsertPaneResult;
export declare function parseMovePaneResult(value: unknown): MovePaneResult;
export declare function parseSwapPaneResult(value: unknown): SwapPaneResult;
export declare function parseAskedEvent(value: unknown): AskedEvent;
export declare function parseWatchEvent(value: unknown, path?: string): WatchEvent;
export declare function parseRenderedFrame(value: unknown): RenderedFrame;
export declare function parseRunResult(value: unknown): RunResult;
export declare function parseAgentRecord(value: unknown, path?: string): AgentRecord;
export declare function parseAgentStateList(value: unknown): AgentStateList;
export declare function isAgentEventType(value: string): value is AgentEventType;
export declare function parseAgentSessionOpenResult(value: unknown): AgentSessionOpenResult;
export declare function parseAgentEmitResult(value: unknown): AgentEmitResult;
