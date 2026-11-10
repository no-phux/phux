import type { AgentEmitOptions, AgentSessionOpenOptions, ExecutionOptions } from "./adapter.js";
import type { AgentEmitResult, AgentEventType, AgentSessionCloseResult, AgentSessionOpenResult } from "./schemas.js";
/** The three AgentSession CLI verbs, so hosts can inject a fake without constructing argv. */
export interface AgentSessionCli {
    agentSessionOpen(target: string, options: AgentSessionOpenOptions): Promise<AgentSessionOpenResult>;
    agentEmit(target: string, type: AgentEventType, options?: AgentEmitOptions): Promise<AgentEmitResult>;
    agentSessionClose(target: string, options?: ExecutionOptions): Promise<AgentSessionCloseResult>;
}
export declare function hasAgentSessionCli(cli: object): cli is AgentSessionCli;
/** Total time `agentEmit` spends retrying an upgrade-sealed stream. */
export declare const UPGRADE_RETRY_BUDGET_MS = 3000;
/**
 * True for exactly the refusal a graceful upgrade gives an append between
 * its cut of the stream and the re-exec (L1 §4.8): `overflow` whose
 * diagnostic says the server is upgrading. Nothing was appended, so the
 * same record is safe to resend; every other `overflow` is not retried.
 */
export declare function isUpgradeSealRefusal(error: unknown): boolean;
/**
 * A graceful upgrade seals the stream from its cut until the re-exec and
 * refuses appends in that window; nothing was appended, so retry it.
 */
export declare function retryUpgradeSeal<T>(run: () => Promise<T>, signal: AbortSignal | undefined): Promise<T>;
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
    private resource;
    private unavailable;
    constructor(cli: AgentSessionCli | object | null, options: AgentSessionEmitterOptions);
    get isOpen(): boolean;
    get isUnavailable(): boolean;
    /** Adopt only an identity the host verified against its own declaration. */
    adopt(target: string, nativeId: string, identity: AgentSessionOpenResult): void;
    bind(target: string | null, nativeId: string, options?: ExecutionOptions): Promise<void>;
    emit(type: AgentEventType, data: Readonly<Record<string, unknown>> | undefined, options?: ExecutionOptions): Promise<void>;
    finish(options?: ExecutionOptions): Promise<void>;
    private open;
}
