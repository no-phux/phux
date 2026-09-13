import type {
  AgentEmitOptions,
  AgentSessionOpenOptions,
  ExecutionOptions,
} from "../../runtime/src/adapter.js";
import {
  AgentSessionEmitter,
  hasAgentSessionCli,
  PhuxCli,
} from "../../runtime/src/adapter.js";
import type {
  AgentEmitResult,
  AgentEventType,
  AgentRecord,
  AgentSessionCloseResult,
  AgentSessionOpenResult,
  AgentStateList,
} from "../../runtime/src/schemas.js";

export type OpenCodeLifecycleState = "idle" | "working";

export interface OpenCodeLifecycleAdapter {
  agentShow(options: ExecutionOptions & { readonly target: string }): Promise<AgentStateList>;
  agentSet(target: string, record: AgentRecord, options?: ExecutionOptions): Promise<AgentRecord>;
  agentClear(target: string, options?: ExecutionOptions): Promise<void>;
  agentSessionOpen?(target: string, options: AgentSessionOpenOptions): Promise<AgentSessionOpenResult>;
  agentEmit?(target: string, type: AgentEventType, options?: AgentEmitOptions): Promise<AgentEmitResult>;
  agentSessionClose?(target: string, options?: ExecutionOptions): Promise<AgentSessionCloseResult>;
}

export interface OpenCodeLifecycleOptions {
  readonly cli?: OpenCodeLifecycleAdapter;
  readonly timeoutMs?: number;
  readonly onError?: (error: unknown) => void;
  readonly target: () => string | undefined;
}

interface OwnedBinding {
  readonly target: string;
  readonly owner: string;
}

/**
 * Best-effort metadata reporter driven only by documented session status,
 * deletion, and plugin disposal signals.
 */
export class OpenCodeLifecycle {
  private readonly cli: OpenCodeLifecycleAdapter;
  private readonly timeoutMs: number;
  private readonly onError: (error: unknown) => void;
  private readonly target: () => string | undefined;
  private readonly states = new Map<string, OpenCodeLifecycleState>();
  private readonly owned = new Map<string, OwnedBinding>();
  private readonly sessions = new Map<string, AgentSessionEmitter>();
  private readonly openedPanes = new Map<string, string>();
  private tail: Promise<void> = Promise.resolve();
  private disposed = false;

  constructor(options: OpenCodeLifecycleOptions) {
    this.cli = options.cli ?? new PhuxCli();
    this.timeoutMs = options.timeoutMs ?? 1_000;
    if (!Number.isSafeInteger(this.timeoutMs) || this.timeoutMs <= 0 || this.timeoutMs > 60_000) {
      throw new RangeError("lifecycle timeoutMs must be an integer from 1 through 60000");
    }
    this.onError = options.onError ?? (() => {});
    this.target = options.target;
  }

  /**
   * A session is alive and should carry this plugin's identity.
   *
   * `state` is recorded for {@link targetSelected}'s fallback but is NOT
   * written to the record: the server derives state from `rules/opencode.toml`,
   * and declaring one would stand that detector down (phux-w7z2.38). The event
   * still matters as a liveness trigger, which is why the signature keeps it.
   */
  observeState(sessionId: string, state: OpenCodeLifecycleState): Promise<void> {
    if (this.disposed) return this.tail;
    this.states.set(sessionId, state);
    return this.enqueue(async () => {
      await this.publish(sessionId);
      await this.emit(sessionId, state === "working" ? "prompt" : "stop");
    });
  }

  /** A tool invocation is an honest working signal if no status event was seen yet. */
  targetSelected(sessionId: string): Promise<void> {
    if (this.disposed) return this.tail;
    if (!this.states.has(sessionId)) this.states.set(sessionId, "working");
    return this.enqueue(() => this.publish(sessionId));
  }

  ask(sessionId: string, data: Readonly<Record<string, unknown>> = { kind: "permission" }): Promise<void> {
    if (this.disposed) return this.tail;
    return this.enqueue(async () => {
      await this.publish(sessionId);
      await this.emit(sessionId, "ask", data);
    });
  }

  toolStart(sessionId: string, toolName: string, toolUseId?: string): Promise<void> {
    if (this.disposed) return this.tail;
    const data = {
      tool_name: toolName,
      ...(toolUseId === undefined ? {} : { tool_use_id: toolUseId }),
    };
    return this.enqueue(async () => {
      await this.publish(sessionId);
      await this.emit(sessionId, "tool_start", data);
    });
  }

  toolEnd(sessionId: string, toolName: string, toolUseId?: string): Promise<void> {
    if (this.disposed) return this.tail;
    const data = {
      tool_name: toolName,
      ...(toolUseId === undefined ? {} : { tool_use_id: toolUseId }),
    };
    return this.enqueue(async () => {
      await this.publish(sessionId);
      await this.emit(sessionId, "tool_end", data);
    });
  }

  deleteSession(sessionId: string): Promise<void> {
    this.states.delete(sessionId);
    return this.enqueue(async () => {
      await this.finishSession(sessionId);
      await this.clearSession(sessionId);
    });
  }

  async dispose(): Promise<void> {
    if (this.disposed) return this.tail;
    this.disposed = true;
    this.states.clear();
    const sessions = [...new Set([...this.owned.keys(), ...this.sessions.keys()])];
    this.enqueue(async () => {
      for (const sessionId of sessions) {
        try {
          await this.finishSession(sessionId);
          await this.clearSession(sessionId);
        } catch (error) {
          // Teardown is best effort per owned session: one unavailable pane
          // must not prevent ownership-safe cleanup of the remaining panes.
          this.onError(error);
        }
      }
    });
    await this.tail;
  }

  settled(): Promise<void> {
    return this.tail;
  }

  private enqueue(operation: () => Promise<void>): Promise<void> {
    this.tail = this.tail.then(operation).catch((error: unknown) => {
      this.onError(error);
    });
    return this.tail;
  }

  private async publish(sessionId: string): Promise<void> {
    if (this.disposed) return;
    const target = this.target();
    const previous = this.owned.get(sessionId);
    if (previous !== undefined && previous.target !== target) {
      await this.clearOwned(previous);
      this.owned.delete(sessionId);
    }
    if (target === undefined) {
      await this.emitter(sessionId).bind(null, sessionId, this.execution());
      return;
    }

    await this.bindSession(sessionId, target);

    // Identity is already declared on this exact pane, and the record no longer
    // carries state, so there is nothing left to say. Rewriting it per turn
    // would actively harm: SET_METADATA replaces the record wholesale, so each
    // write carries `state: "unknown"` and clobbers the server's derivation,
    // publishing a `working -> unknown` edge that `phux agent wait` reads as
    // the agent departing (phux-w7z2.37).
    if (previous !== undefined && previous.target === target) return;

    const binding = { target, owner: `opencode:${sessionId}` };
    // Retain attempted ownership so later teardown still performs an
    // ownership check if a confirmation was lost after phux applied the write.
    this.owned.set(sessionId, binding);
    await this.cli.agentSet(target, lifecycleRecord(binding.owner), this.execution());
  }

  private async clearSession(sessionId: string): Promise<void> {
    const binding = this.owned.get(sessionId);
    if (binding === undefined) return;
    try {
      await this.clearOwned(binding);
    } finally {
      // A teardown signal consumes this ownership attempt whether its
      // best-effort remote cleanup succeeds, fails, or finds a replacement.
      this.owned.delete(sessionId);
    }
  }

  private async clearOwned(binding: OwnedBinding): Promise<void> {
    const projection = await this.cli.agentShow({ target: binding.target, ...this.execution() });
    const pane = projection.agents.find((candidate) => candidate.sources.some((source) => {
      if (source.kind !== "agent_record") return false;
      const owner = parseOwner(source.observed);
      return owner?.name === "opencode" && owner.kind === "opencode" && owner.session === binding.owner;
    }));
    if (pane === undefined) return;
    // agent show accepts broad session/window selectors but reports the
    // resolved pane's canonical selector. Clear exactly that canonical pane.
    await this.cli.agentClear(pane.terminal, this.execution());
  }

  private execution(): Required<Pick<ExecutionOptions, "signal" | "timeoutMs">> {
    return { signal: new AbortController().signal, timeoutMs: this.timeoutMs };
  }

  private emitter(sessionId: string): AgentSessionEmitter {
    const existing = this.sessions.get(sessionId);
    if (existing !== undefined) return existing;
    const created = new AgentSessionEmitter(
      hasAgentSessionCli(this.cli) ? this.cli : null,
      { provider: "opencode", onError: this.onError },
    );
    this.sessions.set(sessionId, created);
    return created;
  }

  private async bindSession(sessionId: string, target: string): Promise<void> {
    const owner = this.openedPanes.get(target);
    if (owner !== undefined && owner !== sessionId) return;
    const previous = [...this.openedPanes.entries()].find((entry) => entry[1] === sessionId);
    if (previous !== undefined && previous[0] !== target) this.openedPanes.delete(previous[0]);
    const emitter = this.emitter(sessionId);
    await emitter.bind(target, sessionId, this.execution());
    if (emitter.isOpen) this.openedPanes.set(target, sessionId);
  }

  private async emit(
    sessionId: string,
    type: AgentEventType,
    data?: Readonly<Record<string, unknown>>,
  ): Promise<void> {
    if (this.disposed) return;
    const target = this.target();
    if (target !== undefined) {
      const owner = this.openedPanes.get(target);
      if (owner !== undefined && owner !== sessionId) return;
    }
    await this.emitter(sessionId).emit(type, data, this.execution());
  }

  private async finishSession(sessionId: string): Promise<void> {
    const emitter = this.sessions.get(sessionId);
    if (emitter === undefined) return;
    await emitter.finish(this.execution());
    this.sessions.delete(sessionId);
    for (const [pane, owner] of this.openedPanes) {
      if (owner === sessionId) this.openedPanes.delete(pane);
    }
  }
}

export interface OpenCodeLifecycleEvent {
  readonly type: string;
  readonly properties: Record<string, unknown>;
}

export function handleLifecycleEvent(lifecycle: OpenCodeLifecycle, event: OpenCodeLifecycleEvent): Promise<void> {
  switch (event.type) {
    case "session.status": {
      const sessionID = event.properties.sessionID;
      const status = event.properties.status;
      if (typeof sessionID !== "string" || status === null || typeof status !== "object") return Promise.resolve();
      const statusType = (status as { readonly type?: unknown }).type;
      if (statusType === "busy") {
        return lifecycle.observeState(sessionID, "working");
      }
      if (statusType === "idle") {
        return lifecycle.observeState(sessionID, "idle");
      }
      return Promise.resolve();
    }
    case "session.idle": {
      const sessionID = event.properties.sessionID;
      return typeof sessionID === "string" ? lifecycle.observeState(sessionID, "idle") : Promise.resolve();
    }
    case "session.deleted": {
      const info = event.properties.info;
      const sessionID = info !== null && typeof info === "object" ? (info as { readonly id?: unknown }).id : undefined;
      return typeof sessionID === "string" ? lifecycle.deleteSession(sessionID) : Promise.resolve();
    }
    case "permission.asked": {
      const sessionID = sessionIdOf(event.properties);
      return sessionID === undefined ? Promise.resolve() : lifecycle.ask(sessionID, permissionAskData(event.properties));
    }
    default:
      return Promise.resolve();
  }
}

function sessionIdOf(properties: Record<string, unknown>): string | undefined {
  if (typeof properties.sessionID === "string") return properties.sessionID;
  if (typeof properties.sessionId === "string") return properties.sessionId;
  const info = properties.info;
  if (info !== null && typeof info === "object") {
    const id = (info as { readonly id?: unknown; readonly sessionID?: unknown }).id;
    if (typeof id === "string") return id;
    const nested = (info as { readonly sessionID?: unknown }).sessionID;
    if (typeof nested === "string") return nested;
  }
  return undefined;
}

function permissionAskData(properties: Record<string, unknown>): Readonly<Record<string, unknown>> {
  const id = typeof properties.id === "string" ? properties.id
    : typeof properties.permissionID === "string" ? properties.permissionID
    : undefined;
  const permission = properties.permission;
  const question = typeof permission === "string" ? permission
    : typeof properties.title === "string" ? properties.title
    : undefined;
  return {
    kind: "permission",
    ...(id === undefined ? {} : { id }),
    ...(question === undefined ? {} : { question }),
  };
}

/**
 * Identity only. A declared `state` outranks the server's derivation for the
 * record's whole lifetime (`docs/spec/L3.md` §3.7, ADR-0046 point 8), so
 * reporting one here stood phux's own `rules/opencode.toml` down on every pane
 * running this plugin — phux shipped the rules and the integration disarmed
 * them (phux-w7z2.38).
 */
function lifecycleRecord(owner: string): AgentRecord {
  return { name: "opencode", kind: "opencode", session: owner };
}

interface OwnerFields {
  readonly name?: string;
  readonly kind?: string;
  readonly session?: string;
}

function parseOwner(observed: string): OwnerFields | null {
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
