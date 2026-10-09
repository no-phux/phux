import type {
  ExtensionAPI,
  ExtensionContext,
  MessageEndEvent,
  MessageUpdateEvent,
  ProjectTrustEvent,
  SessionShutdownEvent,
  SessionStartEvent,
  ToolExecutionEndEvent,
  ToolExecutionStartEvent,
  UIPromptStartEvent,
} from "@earendil-works/pi-coding-agent";

import {
  AgentSessionEmitter,
  hasAgentSessionCli,
  PhuxCli,
  type AgentEmitOptions,
  type AgentSessionOpenOptions,
  type ExecutionOptions,
} from "./adapter.js";
import {
  type AgentEmitResult,
  type AgentEventType,
  type AgentRecord,
  type AgentSessionCloseResult,
  type AgentSessionOpenResult,
  type AgentSessionIdentity,
  type AgentStateList,
} from "./schemas.js";
import type { PhuxTargetSelection, PhuxTargetStore } from "./target-store.js";
import { PiTranscript } from "./transcript.js";
import { normalizeTerminalIdentity } from "./awareness.js";

export interface LifecycleCommandOptions {
  readonly signal: AbortSignal;
  readonly timeoutMs: number;
}

export interface PhuxLifecycleAdapter {
  agentShow(options: LifecycleCommandOptions & { readonly target: string }): Promise<AgentStateList>;
  agentSet(target: string, record: AgentRecord, options: LifecycleCommandOptions): Promise<AgentRecord>;
  agentClear(target: string, options: LifecycleCommandOptions): Promise<void>;
  agentSessionOpen?(target: string, options: AgentSessionOpenOptions): Promise<AgentSessionOpenResult>;
  agentEmit?(target: string, type: AgentEventType, options?: AgentEmitOptions): Promise<AgentEmitResult>;
  agentSessionClose?(target: string, options?: ExecutionOptions): Promise<AgentSessionCloseResult>;
}

export interface LifecycleTimers {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

export interface PhuxLifecycleOptions {
  readonly cli?: PhuxLifecycleAdapter;
  /** Hosting pane only. A selected control target is never identity evidence. */
  readonly hostTerminal?: string;
  readonly debounceMs?: number;
  /** Local deadline for each CLI command and for draining work during shutdown. */
  readonly timeoutMs?: number;
  readonly timers?: LifecycleTimers;
  /** Best-effort failures are reported here; the safe default deliberately does nothing. */
  readonly onError?: (error: unknown) => void;
  /**
   * Append `phux.transcript/v1` entries (ADR-0156) as `provider_raw` records.
   * On unless false; the extension maps `PHUX_AGENT_TRANSCRIPT=0` to false.
   */
  readonly transcript?: boolean;
  /** Include tool output in tool entries (`PHUX_AGENT_TRANSCRIPT=full`); off by default. */
  readonly transcriptToolOutput?: boolean;
  /** The transcript mapper; injectable so tests control its clock. */
  readonly transcriptMapper?: PiTranscript;
}

export class PhuxLifecycleShutdownError extends Error {
  constructor(readonly timeoutMs: number) {
    super(`phux lifecycle shutdown exceeded ${String(timeoutMs)}ms`);
    this.name = "PhuxLifecycleShutdownError";
  }
}

/**
 * Who occupies the pane, never what they are doing. A declared `state` would
 * outrank the server's `pi.toml` derivation (L3 §3.7), and because
 * `SET_METADATA` replaces the record wholesale, a per-transition write would
 * clobber derived state. Only a change of owner or target writes.
 */
interface Binding {
  readonly target: PhuxTargetSelection;
  readonly owner: string;
}

const systemTimers: LifecycleTimers = {
  setTimeout: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

/**
 * Serialized, latest-generation lifecycle writer. The class is host-independent
 * so ordering, failures, and shutdown behavior can be tested without Pi.
 */
export class PhuxLifecycle {
  private readonly cli: PhuxLifecycleAdapter;
  private readonly debounceMs: number;
  private readonly timeoutMs: number;
  private readonly timers: LifecycleTimers;
  private readonly onError: (error: unknown) => void;
  private readonly session: AgentSessionEmitter;
  private readonly inFlight = new Set<AbortController>();
  private timer: unknown;
  private tail: Promise<void> = Promise.resolve();
  private generation = 0;
  private active = false;
  private preserveOnStop = false;
  private abandoned = false;
  private owner: string | null = null;
  private sessionId: string | null = null;
  private target: PhuxTargetSelection | null = null;
  private desired: Binding | null = null;
  private applied: Binding | null = null;
  /** A target on which a write was attempted; ownership is still checked before clearing. */
  private owned: Binding | null = null;

  constructor(options: PhuxLifecycleOptions = {}) {
    this.cli = options.cli ?? new PhuxCli();
    this.debounceMs = options.debounceMs ?? 25;
    this.timeoutMs = options.timeoutMs ?? 1_000;
    if (!Number.isSafeInteger(this.debounceMs) || this.debounceMs < 0) {
      throw new RangeError("debounceMs must be a non-negative safe integer");
    }
    if (!Number.isSafeInteger(this.timeoutMs) || this.timeoutMs <= 0) {
      throw new RangeError("timeoutMs must be a positive safe integer");
    }
    this.timers = options.timers ?? systemTimers;
    this.onError = options.onError ?? (() => {});
    this.session = new AgentSessionEmitter(
      hasAgentSessionCli(this.cli) ? this.cli : null,
      { provider: "pi", onError: this.onError },
    );
  }

  start(sessionId: string, target: PhuxTargetSelection | null, reload = false): void {
    this.active = true;
    this.preserveOnStop = false;
    this.abandoned = false;
    this.sessionId = sessionId;
    this.owner = `pi:${sessionId}`;
    this.target = target;
    this.desired = this.binding();
    this.generation += 1;
    if (reload) {
      const binding = this.desired;
      const generation = this.generation;
      // A previous version may have bound the selected sibling, not this host.
      // Keep writes disabled until both declaration and child identity agree.
      this.desired = null;
      if (binding !== null) this.enqueueWork(() => this.restoreReload(binding, sessionId, generation));
      return;
    }
    this.enqueueSessionBind();
    this.schedule();
  }

  setTarget(target: PhuxTargetSelection | null): void {
    if (!this.active) return;
    this.target = target;
    this.enqueueSessionBind();
    this.transition();
  }

  /**
   * Append one AgentSession record. No-ops when this instance did not open
   * the session (missing verb, failed open, or no pane yet). Never writes
   * detector `state`.
   */
  emit(type: AgentEventType, data?: Readonly<Record<string, unknown>>): void {
    if (!this.active || this.session.isUnavailable) return;
    this.enqueueWork(async () => {
      if (this.abandoned || (!this.active && this.preserveOnStop)) return;
      await this.runCommand((options) => this.session.emit(type, data, options));
    });
  }


  /** Stop timers and either preserve on reload or clear only our declaration. */
  async shutdown(reload = false): Promise<void> {
    this.cancelTimer();
    this.active = false;
    this.preserveOnStop = reload;
    this.generation += 1;
    this.abortInFlight();
    if (!reload) {
      this.desired = null;
      this.enqueue();
      if (!this.session.isUnavailable) {
        this.enqueueWork(async () => {
          await this.runCommand((options) => this.session.finish(options));
        });
      }
    }
    if (!await this.waitForTail()) {
      this.abandoned = true;
      this.generation += 1;
      this.abortInFlight();
      this.onError(new PhuxLifecycleShutdownError(this.timeoutMs));
    }
  }

  /** Wait for all work that is currently queued (primarily for tests). */
  async settled(): Promise<void> {
    await this.tail;
  }

  private reloadIsCurrent(generation: number): boolean {
    return this.active && !this.abandoned && generation === this.generation;
  }

  private async restoreReload(binding: Binding, sessionId: string, generation: number): Promise<void> {
    if (!this.reloadIsCurrent(generation)) return;
    const projection = await this.runCommand((options) =>
      this.cli.agentShow({ target: binding.target.selector, ...options }));
    if (!this.reloadIsCurrent(generation)) return;
    const restored = reloadBinding(binding, projection, sessionId);
    if (restored.session !== null) {
      this.session.adopt(binding.target.selector, sessionId, {
        schema_version: 1, parent: binding.target.selector, ...restored.session,
      });
    } else {
      await this.runCommand((options) => this.session.bind(binding.target.selector, sessionId, options));
    }
    if (!this.reloadIsCurrent(generation)) return;
    this.desired = binding;
    if (restored.ownsRecord) {
      this.applied = binding;
      this.owned = binding;
    }
    await this.reconcile();
  }

  private transition(): void {
    this.desired = this.binding();
    this.generation += 1;
    this.schedule();
  }

  private binding(): Binding | null {
    if (this.owner === null || this.target === null) return null;
    return { target: this.target, owner: this.owner };
  }

  private enqueueSessionBind(): void {
    if (this.session.isUnavailable) return;
    const target = this.target;
    const sessionId = this.sessionId;
    this.enqueueWork(async () => {
      if (this.abandoned || (!this.active && this.preserveOnStop)) return;
      if (sessionId === null) return;
      await this.runCommand((options) =>
        this.session.bind(target?.selector ?? null, sessionId, options));
    });
  }

  private schedule(): void {
    this.cancelTimer();
    this.timer = this.timers.setTimeout(() => {
      this.timer = undefined;
      this.enqueue();
    }, this.debounceMs);
  }

  private cancelTimer(): void {
    if (this.timer === undefined) return;
    this.timers.clearTimeout(this.timer);
    this.timer = undefined;
  }

  private enqueue(): void {
    this.enqueueWork(() => this.reconcile());
  }

  private enqueueWork(operation: () => Promise<void>): void {
    this.tail = this.tail.then(operation).catch((error: unknown) => {
      this.onError(error);
    });
  }

  private async reconcile(): Promise<void> {
    while (true) {
      if (this.abandoned || (!this.active && this.preserveOnStop)) return;
      const generation = this.generation;
      const desired = this.desired;

      if (this.owned !== null && (desired === null || !sameOwnerTarget(this.owned, desired))) {
        const old = this.owned;
        const released = await this.clearOwned(old);
        if (!released) return;
        if (this.owned === old) this.owned = null;
        if (this.applied !== null && sameOwnerTarget(this.applied, old)) this.applied = null;
        if (this.abandoned || (!this.active && this.preserveOnStop)) return;
        if (generation !== this.generation) continue;
      }

      if (desired === null) return;
      if (this.applied !== null && sameOwnerTarget(this.applied, desired)) return;

      this.owned = desired;
      try {
        await this.runCommand((options) =>
          this.cli.agentSet(desired.target.selector, lifecycleRecord(desired), options));
      } catch (error) {
        this.onError(error);
        return;
      }
      this.applied = desired;
      if (this.abandoned || (!this.active && this.preserveOnStop)) return;
      if (generation !== this.generation) continue;
      return;
    }
  }

  private async clearOwned(binding: Binding): Promise<boolean> {
    try {
      const projection = await this.runCommand((options) =>
        this.cli.agentShow({ target: binding.target.selector, ...options }));
      const pane = projection.agents.find((candidate) =>
        candidate.terminal === binding.target.selector &&
        candidate.session === binding.target.session &&
        candidate.window === binding.target.window);
      const source = pane?.sources.find((candidate) => candidate.kind === "agent_record");
      if (source === undefined) return true;
      if (!ownsDeclaration(source.observed, binding.owner)) {
        return true;
      }
      await this.runCommand((options) => this.cli.agentClear(binding.target.selector, options));
      return true;
    } catch (error) {
      // Lifecycle reporting must never make Pi startup, transitions, or exit fail.
      this.onError(error);
      return false;
    }
  }

  private async runCommand<T>(body: (options: LifecycleCommandOptions) => Promise<T>): Promise<T> {
    const controller = new AbortController();
    this.inFlight.add(controller);
    try {
      return await body({ signal: controller.signal, timeoutMs: this.timeoutMs });
    } finally {
      this.inFlight.delete(controller);
    }
  }

  private abortInFlight(): void {
    for (const controller of this.inFlight) controller.abort();
  }

  private async waitForTail(): Promise<boolean> {
    return new Promise<boolean>((resolve) => {
      let done = false;
      let timeout: unknown;
      const finish = (completed: boolean): void => {
        if (done) return;
        done = true;
        if (timeout !== undefined) this.timers.clearTimeout(timeout);
        resolve(completed);
      };
      timeout = this.timers.setTimeout(() => finish(false), this.timeoutMs);
      void this.tail.then(() => finish(true), () => finish(true));
    });
  }
}

export interface RegisteredPhuxLifecycle {
  readonly lifecycle: PhuxLifecycle;
}

/** Register Pi lifecycle hooks around the already-shared target store. */
export function registerPhuxLifecycle(
  pi: ExtensionAPI,
  store: PhuxTargetStore,
  options: PhuxLifecycleOptions = {},
): RegisteredPhuxLifecycle {
  const lifecycle = new PhuxLifecycle(options);
  const host = normalizeTerminalIdentity(options.hostTerminal);

  pi.on("session_start", (event: SessionStartEvent, ctx: ExtensionContext) => {
    // The extension's earlier startup handler refreshed this inventory. Resolve
    // the hosting pane independently even when no control target is selected.
    // No inventory match means no identity mutation, never a focused-pane fallback.
    lifecycle.start(
      ctx.sessionManager.getSessionId(),
      host === null ? null : store.selectionFor(host),
      event.reason === "reload",
    );
  });
  // Per-turn events feed the AgentSession stream and never write `state`.
  // Transcript entries ride `provider_raw` beside the typed records.
  const transcript = options.transcript === false ? null : options.transcriptMapper ??
    new PiTranscript(undefined, options.transcriptToolOutput === true);
  const emitTranscript = (records: readonly Readonly<Record<string, unknown>>[]): void => {
    for (const data of records) lifecycle.emit("provider_raw", data);
  };
  pi.on("agent_start", () => lifecycle.emit("prompt"));
  if (transcript !== null) {
    pi.on("message_update", (event: MessageUpdateEvent) => {
      emitTranscript(transcript.messageUpdate(event.message, event.assistantMessageEvent?.type));
    });
    pi.on("message_end", (event: MessageEndEvent) => {
      emitTranscript(transcript.messageEnd(event.message));
    });
  }
  pi.on("tool_execution_start", (event: ToolExecutionStartEvent) => {
    lifecycle.emit("tool_start", { tool_name: event.toolName, tool_use_id: event.toolCallId });
    if (transcript !== null) emitTranscript(transcript.toolStart(event));
  });
  pi.on("tool_execution_end", (event: ToolExecutionEndEvent) => {
    lifecycle.emit("tool_end", {
      tool_name: event.toolName,
      tool_use_id: event.toolCallId,
      ok: !event.isError,
    });
    if (transcript !== null) emitTranscript(transcript.toolEnd(event));
  });
  pi.on("ui_prompt_start", (event: UIPromptStartEvent) => {
    lifecycle.emit("ask", { kind: event.kind, ...(event.title === undefined ? {} : { question: event.title }) });
  });
  pi.on("project_trust", (event: ProjectTrustEvent) => {
    lifecycle.emit("ask", { kind: "trust", question: event.cwd });
    // Do not own the trust decision; report blocked and let Pi continue.
    return { trusted: "undecided" as const };
  });
  pi.on("agent_settled", () => lifecycle.emit("stop"));
  pi.on("session_shutdown", async (event: SessionShutdownEvent) => {
    await lifecycle.shutdown(event.reason === "reload");
  });

  return { lifecycle };
}

function reloadBinding(
  binding: Binding, projection: AgentStateList, sessionId: string,
): { ownsRecord: boolean; session: AgentSessionIdentity | null } {
  const pane = projection.agents.find((candidate) =>
    candidate.terminal === binding.target.selector &&
    candidate.session === binding.target.session && candidate.window === binding.target.window);
  if (pane === undefined || pane.agent_session === undefined) {
    throw new Error("Cannot verify Pi hosting identity on reload; leaving metadata and sessions untouched");
  }
  const source = pane.sources.find((candidate) => candidate.kind === "agent_record");
  const ownsRecord = ownsDeclaration(source?.observed, binding.owner);
  if (source !== undefined && !ownsRecord) {
    throw new Error("Pi hosting declaration belongs to another owner; refusing reload adoption");
  }
  const session = pane.agent_session;
  if (session !== null && (!ownsRecord || session.provider !== "pi" || session.native_id !== sessionId)) {
    throw new Error("Pi hosting AgentSession belongs to another owner; refusing reload adoption");
  }
  return { ownsRecord, session };
}

function ownsDeclaration(observed: string | undefined, owner: string): boolean {
  if (observed === undefined) return false;
  const record = parseOwnership(observed);
  return record?.name === "pi" && record.kind === "pi" && record.session === owner;
}

interface OwnershipFields {
  readonly name?: string;
  readonly kind?: string;
  readonly session?: string;
}

function parseOwnership(observed: string): OwnershipFields | null {
  try {
    const value: unknown = JSON.parse(observed);
    if (value === null || typeof value !== "object" || Array.isArray(value)) return null;
    const row = value as Record<string, unknown>;
    return {
      ...(typeof row.name === "string" ? { name: row.name } : {}),
      ...(typeof row.kind === "string" ? { kind: row.kind } : {}),
      ...(typeof row.session === "string" ? { session: row.session } : {}),
    };
  } catch {
    return null;
  }
}

function lifecycleRecord(binding: Binding): AgentRecord {
  // Identity only. `state` and `attention` are the server's to derive.
  return { name: "pi", kind: "pi", session: binding.owner };
}

function sameOwnerTarget(left: Binding, right: Binding): boolean {
  return left.owner === right.owner &&
    left.target.selector === right.target.selector &&
    left.target.session === right.target.session &&
    left.target.window === right.target.window;
}
