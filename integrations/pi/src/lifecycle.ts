import type {
  ExtensionAPI,
  ExtensionContext,
  SessionShutdownEvent,
  SessionStartEvent,
} from "@earendil-works/pi-coding-agent";

import { PhuxCli } from "./adapter.js";
import { type AgentRecord, type AgentStateList } from "./schemas.js";
import type { PhuxTargetSelection, PhuxTargetStore } from "./target-store.js";

export interface LifecycleCommandOptions {
  readonly signal: AbortSignal;
  readonly timeoutMs: number;
}

export interface PhuxLifecycleAdapter {
  agentShow(options: LifecycleCommandOptions & { readonly target: string }): Promise<AgentStateList>;
  agentSet(target: string, record: AgentRecord, options: LifecycleCommandOptions): Promise<AgentRecord>;
  agentClear(target: string, options: LifecycleCommandOptions): Promise<void>;
}

export interface LifecycleTimers {
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

export interface PhuxLifecycleOptions {
  readonly cli?: PhuxLifecycleAdapter;
  readonly debounceMs?: number;
  /** Local deadline for each CLI command and for draining work during shutdown. */
  readonly timeoutMs?: number;
  readonly timers?: LifecycleTimers;
  /** Best-effort failures are reported here; the safe default deliberately does nothing. */
  readonly onError?: (error: unknown) => void;
}

export class PhuxLifecycleShutdownError extends Error {
  constructor(readonly timeoutMs: number) {
    super(`phux lifecycle shutdown exceeded ${String(timeoutMs)}ms`);
    this.name = "PhuxLifecycleShutdownError";
  }
}

/**
 * What this integration declares: WHO occupies the pane, never WHAT they are
 * doing.
 *
 * A declared `state` outranks the server's derivation for the record's whole
 * lifetime (`docs/spec/L3.md` §3.7, ADR-0046 point 8), so reporting one here
 * stood phux's own `pi.toml` manifest down on every pane running this
 * extension — phux shipped the rules and the integration disarmed them
 * (phux-w7z2.38).
 *
 * State is deliberately absent from the binding, not merely from the record.
 * `SET_METADATA` replaces the record wholesale, so an identity write carries
 * `state: "unknown"`; if a lifecycle transition still produced a *write*, it
 * would clobber the derived state and publish a `working -> unknown` edge that
 * `phux agent wait` reads as the agent departing (phux-w7z2.37, the regression
 * the Claude shim hit first). Only a change of owner or target writes.
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
  private readonly inFlight = new Set<AbortController>();
  private timer: unknown;
  private tail: Promise<void> = Promise.resolve();
  private generation = 0;
  private active = false;
  private preserveOnStop = false;
  private abandoned = false;
  private owner: string | null = null;
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
  }

  start(sessionId: string, target: PhuxTargetSelection | null, reload = false): void {
    this.active = true;
    this.preserveOnStop = false;
    this.abandoned = false;
    this.owner = `pi:${sessionId}`;
    this.target = target;
    this.desired = this.binding();
    this.generation += 1;
    if (reload) {
      // The previous extension instance deliberately left this declaration in
      // place. Adopt it without a clear/set flicker; the next real transition
      // will confirm the current state.
      this.applied = this.desired;
      this.owned = this.desired;
      return;
    }
    this.schedule();
  }

  setTarget(target: PhuxTargetSelection | null): void {
    if (!this.active) return;
    this.target = target;
    this.transition();
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

  private transition(): void {
    this.desired = this.binding();
    this.generation += 1;
    this.schedule();
  }

  private binding(): Binding | null {
    if (this.owner === null || this.target === null) return null;
    return { target: this.target, owner: this.owner };
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
    this.tail = this.tail.then(() => this.reconcile()).catch((error: unknown) => {
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
      if (this.applied !== null && sameBinding(this.applied, desired)) return;

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
      const ownership = parseOwnership(source.observed);
      if (ownership?.name !== "pi" || ownership.kind !== "pi" || ownership.session !== binding.owner) {
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
  let unsubscribe = store.subscribe((selection) => lifecycle.setTarget(selection));

  pi.on("session_start", (event: SessionStartEvent, ctx: ExtensionContext) => {
    lifecycle.start(
      ctx.sessionManager.getSessionId(),
      store.snapshot.availability === "available" ? store.snapshot.selection : null,
      event.reason === "reload",
    );
  });
  // No `agent_start` / `agent_settled` subscription: this integration declares
  // identity, and the server derives state from `rules/pi.toml` (phux-w7z2.38).
  // Subscribing to write nothing would only cost a debounce timer per turn.
  pi.on("session_shutdown", async (event: SessionShutdownEvent) => {
    unsubscribe();
    unsubscribe = () => {};
    await lifecycle.shutdown(event.reason === "reload");
  });

  return { lifecycle };
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

/**
 * Identity is the whole binding, so this is `sameOwnerTarget`. Kept as a
 * distinct name because the reconciler's two call sites mean different things:
 * one asks "is the pane we own still the pane we want", the other "is a write
 * needed at all".
 */
function sameBinding(left: Binding, right: Binding): boolean {
  return sameOwnerTarget(left, right);
}
