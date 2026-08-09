import type { Hooks } from "@opencode-ai/plugin";

import type { ExecutionOptions } from "../../pi/src/adapter.js";
import { PhuxCli } from "../../pi/src/adapter.js";
import type { AgentRecord, AgentStateList } from "../../pi/src/schemas.js";

export type OpenCodeLifecycleState = "idle" | "working";

export interface OpenCodeLifecycleAdapter {
  agentShow(options: ExecutionOptions & { readonly target: string }): Promise<AgentStateList>;
  agentSet(target: string, record: AgentRecord, options?: ExecutionOptions): Promise<AgentRecord>;
  agentClear(target: string, options?: ExecutionOptions): Promise<void>;
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
    return this.enqueue(() => this.publish(sessionId));
  }

  /** A tool invocation is an honest working signal if no status event was seen yet. */
  targetSelected(sessionId: string): Promise<void> {
    return this.observeState(sessionId, this.states.get(sessionId) ?? "working");
  }

  deleteSession(sessionId: string): Promise<void> {
    this.states.delete(sessionId);
    return this.enqueue(async () => {
      await this.clearSession(sessionId);
    });
  }

  async dispose(): Promise<void> {
    if (this.disposed) return this.tail;
    this.disposed = true;
    this.states.clear();
    const sessions = [...this.owned.keys()];
    this.enqueue(async () => {
      for (const sessionId of sessions) {
        try {
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
    if (target === undefined) return;

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
}

export function handleLifecycleEvent(lifecycle: OpenCodeLifecycle, event: Parameters<NonNullable<Hooks["event"]>>[0]["event"]): Promise<void> {
  switch (event.type) {
    case "session.status":
      if (event.properties.status.type === "busy") {
        return lifecycle.observeState(event.properties.sessionID, "working");
      }
      if (event.properties.status.type === "idle") {
        return lifecycle.observeState(event.properties.sessionID, "idle");
      }
      return Promise.resolve();
    case "session.idle":
      return lifecycle.observeState(event.properties.sessionID, "idle");
    case "session.deleted":
      return lifecycle.deleteSession(event.properties.info.id);
    default:
      return Promise.resolve();
  }
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
