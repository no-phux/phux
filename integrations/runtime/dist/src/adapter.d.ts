import { type ProcessRunner } from "./runner.js";
import { type AgentEmitResult, type AgentEventType, type AgentRecord, type AgentSessionCloseResult, type AgentSessionOpenResult, type AgentStateList, type AskedEvent, type CreateResult, type InsertPaneResult, type LaunchResult, type MovePaneResult, type RenderedFrame, type RunResult, type ScreenState, type SessionList, type SpawnResult, type SwapPaneResult, type TagRow, type WatchEvent } from "./schemas.js";
export type { AgentEmitResult, AgentEventType, AgentSessionCloseResult, AgentSessionOpenResult, } from "./schemas.js";
export { AGENT_EVENT_TYPES, isAgentEventType } from "./schemas.js";
export interface PhuxCliOptions {
    readonly executable?: string;
    readonly socket?: string;
    readonly cwd?: string;
    readonly env?: NodeJS.ProcessEnv;
    readonly runner?: ProcessRunner;
    readonly maxStdoutBytes?: number;
    readonly maxStderrBytes?: number;
}
export interface ExecutionOptions {
    /** Abort this local subprocess invocation. */
    readonly signal?: AbortSignal;
    /** Kill this local subprocess if it has not exited within this many ms. */
    readonly timeoutMs?: number;
}
export interface SnapshotOptions extends ExecutionOptions {
    readonly target?: string;
    /** true or zero means all retained history; a positive number bounds it. */
    readonly scrollback?: boolean | number;
    readonly cells?: boolean;
}
export interface WaitOptions extends ExecutionOptions {
    readonly target?: string;
    readonly until?: string;
    readonly idleMs?: number;
    /** phux's own wait deadline, in seconds (distinct from local timeoutMs). */
    readonly phuxTimeoutSeconds?: number;
}
export type WaitOutcome = {
    readonly outcome: "satisfied";
    readonly screen: ScreenState;
} | {
    readonly outcome: "timed_out";
    readonly screen: ScreenState;
};
export interface CreateOptions extends ExecutionOptions {
    readonly cwd?: string;
    readonly command?: readonly string[];
}
export interface RunOptions extends ExecutionOptions {
    /** phux's own sentinel deadline, in seconds (distinct from local timeoutMs). */
    readonly phuxTimeoutSeconds?: number;
}
export interface AgentTargetOptions extends ExecutionOptions {
    readonly target: string;
}
export interface AgentSessionOpenOptions extends ExecutionOptions {
    readonly provider: string;
    readonly nativeId?: string;
}
export interface AgentEmitOptions extends ExecutionOptions {
    readonly data?: Readonly<Record<string, unknown>>;
}
export type SplitDirection = "horizontal" | "vertical";
export interface PlacementOptions {
    readonly target?: string;
    readonly split?: SplitDirection;
    readonly ratio?: number;
}
export interface SpawnOptions extends ExecutionOptions, PlacementOptions {
    readonly satellite?: string;
    readonly cwd?: string;
    readonly command?: readonly string[];
}
export interface LaunchOptions extends ExecutionOptions, PlacementOptions {
    readonly cwd?: string;
    readonly extra?: readonly string[];
}
export interface SpatialOptions extends ExecutionOptions {
    readonly direction?: SplitDirection;
    readonly ratio?: number;
}
export interface RenderedSnapshotOptions extends ExecutionOptions {
    readonly session?: string;
    readonly cols: number;
    readonly rows: number;
}
export interface AskOptions extends ExecutionOptions {
    readonly id?: string;
    readonly suggestions?: readonly string[];
    readonly elapsedSeconds?: number;
}
export type TerminalSignal = "interrupt" | "freeze" | "resume" | "terminate" | "kill";
export type TagAction = "ls" | "add" | "rm";
export interface WatchOptions extends ExecutionOptions {
    readonly target: string;
    /** Required collection window. The streaming CLI is always stopped after this bound. */
    readonly durationMs: number;
    readonly maxEvents: number;
}
export interface WatchCollection {
    readonly events: readonly WatchEvent[];
    readonly truncated: boolean;
    readonly ended: boolean;
}
export interface PhuxProbe {
    readonly available: boolean;
    readonly version?: string;
    readonly rawVersion?: string;
    readonly reason?: string;
}
export declare const MINIMUM_PHUX_VERSION = "0.1.0";
export declare class PhuxCli {
    readonly executable: string;
    readonly socket: string | undefined;
    private readonly cwd;
    private readonly env;
    private readonly runner;
    private readonly maxStdoutBytes;
    private readonly maxStderrBytes;
    constructor(options?: PhuxCliOptions);
    probe(options?: ExecutionOptions): Promise<PhuxProbe>;
    ls(options?: ExecutionOptions): Promise<SessionList>;
    /** Inventory panes and their owning session through the documented agent CLI projection. */
    agentList(options?: ExecutionOptions): Promise<AgentStateList>;
    create(name: string, options?: CreateOptions): Promise<CreateResult>;
    spawn(options?: SpawnOptions): Promise<SpawnResult>;
    launch(integration: string, options?: LaunchOptions): Promise<LaunchResult>;
    insertPane(target: string, newPane: string, options?: SpatialOptions): Promise<InsertPaneResult>;
    movePane(source: string, target: string, options?: SpatialOptions): Promise<MovePaneResult>;
    swapPane(first: string, second: string, options?: ExecutionOptions): Promise<SwapPaneResult>;
    /** Read one pane's public projection, including declared-record provenance. */
    agentShow(options: AgentTargetOptions): Promise<AgentStateList>;
    /** Write and parse the CLI's confirmed whole-record response. */
    agentSet(target: string, record: AgentRecord, options?: ExecutionOptions): Promise<AgentRecord>;
    /** Clear a declaration and require the CLI's confirmed tombstone response. */
    agentClear(target: string, options?: ExecutionOptions): Promise<void>;
    /** Open an AgentSession bound to a pane; the caller becomes its producer. */
    agentSessionOpen(target: string, options: AgentSessionOpenOptions): Promise<AgentSessionOpenResult>;
    /** Append one closed-type record. No-op-refused by the server if this client is not the opener. */
    agentEmit(target: string, type: AgentEventType, options?: AgentEmitOptions): Promise<AgentEmitResult>;
    /** Close a pane's AgentSession; the parent pane is untouched. */
    agentSessionClose(target: string, options?: ExecutionOptions): Promise<AgentSessionCloseResult>;
    renderedSnapshot(options: RenderedSnapshotOptions): Promise<RenderedFrame>;
    snapshot(options?: SnapshotOptions): Promise<ScreenState>;
    wait(options?: WaitOptions): Promise<WaitOutcome>;
    run(target: string, command: readonly string[], options?: RunOptions): Promise<RunResult>;
    sendKeys(target: string, keys: readonly string[], options?: ExecutionOptions): Promise<void>;
    kill(target: string, options?: ExecutionOptions): Promise<void>;
    signal(target: string, signal: TerminalSignal, options?: ExecutionOptions): Promise<void>;
    tag(action: TagAction, target: string, tags?: readonly string[], options?: ExecutionOptions): Promise<readonly TagRow[]>;
    ask(target: string, question: string, options?: AskOptions): Promise<AskedEvent>;
    watch(options: WatchOptions): Promise<WatchCollection>;
    private jsonCommand;
    private completed;
    private execute;
    private throwTermination;
    private withSocket;
    private pushSocket;
}
/** The three AgentSession CLI verbs, so hosts can inject a fake without constructing argv. */
export interface AgentSessionCli {
    agentSessionOpen(target: string, options: AgentSessionOpenOptions): Promise<AgentSessionOpenResult>;
    agentEmit(target: string, type: AgentEventType, options?: AgentEmitOptions): Promise<AgentEmitResult>;
    agentSessionClose(target: string, options?: ExecutionOptions): Promise<AgentSessionCloseResult>;
}
export declare function hasAgentSessionCli(cli: object): cli is AgentSessionCli;
/**
 * True when `phux agent session open` is missing: an older binary without the
 * verb, or a server that refuses with `unsupported_server`. Emit must then
 * fail closed; identity-only `agent set` still runs.
 */
export declare function isAgentSessionUnsupported(error: unknown): boolean;
export interface AgentSessionEmitterOptions {
    readonly provider: string;
    readonly onError?: (error: unknown) => void;
}
/**
 * Open once per pane, emit only after a successful open, close the session we
 * opened. A missing verb marks the emitter unavailable for the rest of its
 * life so later hooks do not retry a command the server does not have.
 */
export declare class AgentSessionEmitter {
    private readonly cli;
    private readonly provider;
    private readonly onError;
    private target;
    private nativeId;
    private opened;
    private unavailable;
    constructor(cli: AgentSessionCli | object | null, options: AgentSessionEmitterOptions);
    get isOpen(): boolean;
    get isUnavailable(): boolean;
    /** Take over a session this process left open (extension reload). */
    adopt(target: string | null): void;
    bind(target: string | null, nativeId: string, options?: ExecutionOptions): Promise<void>;
    emit(type: AgentEventType, data: Readonly<Record<string, unknown>> | undefined, options?: ExecutionOptions): Promise<void>;
    finish(options?: ExecutionOptions): Promise<void>;
    private open;
}
