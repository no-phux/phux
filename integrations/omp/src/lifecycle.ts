import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { AgentSessionEmitter, type PhuxCli, type ExecutionOptions } from "../../runtime/src/adapter.js";
import type { AgentEventType, AgentPane, AgentRecord, AgentStateList } from "../../runtime/src/schemas.js";

type Cli = Pick<PhuxCli, "agentShow" | "agentSet" | "agentClear" | "agentSessionOpen" | "agentEmit" | "agentSessionClose">;
interface Binding {
  id: string;
  pane: AgentPane;
  child: string | null;
  emitter: AgentSessionEmitter;
}
const declaration = (id: string): AgentRecord => ({ name: "omp", kind: "omp", session: `omp:${id}` });

function owns(pane: AgentPane, id: string): boolean {
  const sources = pane.sources.filter(source => source.kind === "agent_record");
  if (sources.length !== 1) return false;
  try {
    const record = JSON.parse(sources[0]!.observed);
    return record.name === "omp" && record.kind === "omp" && record.session === `omp:${id}`;
  } catch { return false; }
}

function hostPane(projection: AgentStateList, host: string): AgentPane {
  const matches = projection.agents.filter(pane => pane.terminal === host);
  const pane = matches[0];
  if (matches.length !== 1 || !pane?.session || !pane.window || pane.agent_session === undefined) {
    throw new Error("Unproven OMP hosting projection");
  }
  return pane;
}

function samePlace(left: AgentPane, right: AgentPane): boolean {
  return left.terminal === right.terminal && left.session === right.session && left.window === right.window;
}

const UNBOUND_FIELDS = new Set(["name", "kind", "session", "state", "attention"]);
const OBSERVED_STATES = new Set(["unknown", "idle", "working", "blocked", "done"]);

/** Explicit cold-bootstrap policy, not an inference about the record's author. */
function unboundDeclaration(observed: string): boolean {
  try {
    const record: unknown = JSON.parse(observed);
    if (record === null || typeof record !== "object" || Array.isArray(record)) return false;
    // Flat allowed values make every JSON property token a top-level field;
    // reject duplicate keys rather than letting JSON.parse erase contradictions.
    const properties = observed.match(/"(?:\\.|[^"\\])*"\s*:/g) ?? [];
    if (properties.length !== Object.keys(record).length) return false;
    return unboundIdentity(record as Record<string, unknown>);
  } catch { return false; }
}

function unboundIdentity(record: Record<string, unknown>): boolean {
  if (Object.keys(record).some(key => !UNBOUND_FIELDS.has(key))) return false;
  if (record.name !== "omp" || record.kind !== "omp") return false;
  if (record.session != null || record.attention != null) return false;
  return record.state == null || OBSERVED_STATES.has(record.state as string);
}

/** Return whether an identity-only write is needed, even for an existing bare
 * record. Owned-child adoption remains a separate, strict proof. */
function needsDeclaration(pane: AgentPane, id: string, cold: boolean): boolean {
  if (owns(pane, id)) return false;
  if (pane.agent_session !== null) throw new Error("Unowned OMP child");
  const sources = pane.sources.filter(source => source.kind === "agent_record");
  if (sources.length === 0) return true;
  if (cold && sources.length === 1 && unboundDeclaration(sources[0]!.observed)) return true;
  throw new Error("Foreign or malformed OMP declaration");
}

/** Host-specific serialized writer. An uncertain operation disables this instance;
 * no mutation is retried, including after navigation. Tools are independent. */
export class OmpLifecycle {
  private tail: Promise<void> = Promise.resolve();
  private generation = 0;
  private id: string | undefined;
  private binding: Binding | undefined;
  private disabled = false;
  private stopped = false;
  private declarationOnly = false;
  private deadline = Infinity;
  private pending: AbortController | undefined;

  constructor(private readonly cli: Cli, private readonly host?: string, private readonly timeoutMs = 250) {}

  navigate(id: string, coldStart = false): Promise<void> {
    if (this.stopped || this.disabled || !this.host) return this.tail;
    if (this.id === id) return this.tail;
    const cold = coldStart && this.id === undefined && !this.declarationOnly;
    this.id = id;
    const generation = ++this.generation;
    return this.enqueue(async () => {
      if (!this.current(id, generation)) return;
      if (this.binding?.id === id) return;
      await this.rotateDeclaration(id);
      if (this.current(id, generation)) await this.bind(id, generation, cold);
    });
  }

  fallback(): Promise<void> {
    this.declarationOnly = true;
    ++this.generation;
    return this.enqueue(() => this.closeChild());
  }

  emit(id: string, generation: number, type: AgentEventType, data?: Readonly<Record<string, unknown>>): Promise<void> {
    return this.enqueue(async () => {
      if (this.declarationOnly || !this.current(id, generation)) return;
      const binding = this.binding;
      if (!binding?.emitter.isOpen) return;
      await this.verify(binding);
      if (!this.current(id, generation)) return;
      await binding.emitter.emit(type, data);
    });
  }

  token(): { id: string; generation: number } | undefined {
    return this.id === undefined ? undefined : { id: this.id, generation: this.generation };
  }

  async shutdown(): Promise<void> {
    if (this.stopped) return;
    this.stopped = true;
    ++this.generation;
    // OMP caps ALL shutdown callbacks at 2 seconds. This adapter uses at most
    // 1.2 seconds total, including queued work, not one budget per subprocess.
    this.deadline = Date.now() + 1_200;
    let timer: ReturnType<typeof setTimeout>;
    await Promise.race([
      this.enqueue(() => this.retire()),
      new Promise<void>(resolve => { timer = setTimeout(() => {
        this.disabled = true;
        this.pending?.abort();
        resolve();
      }, 1_200); }),
    ]);
    clearTimeout(timer!);
  }

  private current(id: string, generation: number): boolean {
    return !this.stopped && !this.disabled && this.id === id && this.generation === generation;
  }

  private enqueue(operation: () => Promise<void>): Promise<void> {
    this.tail = this.tail.then(async () => { if (!this.disabled) await operation(); })
      .catch(() => { this.disabled = true; });
    return this.tail;
  }

  private async command<T>(operation: (options: ExecutionOptions) => Promise<T>): Promise<T> {
    if (this.disabled || Date.now() >= this.deadline) throw new Error("OMP lifecycle unavailable");
    const controller = new AbortController();
    this.pending = controller;
    const timeoutMs = Math.min(this.timeoutMs, this.deadline - Date.now());
    let timer: ReturnType<typeof setTimeout>;
    try {
      return await Promise.race([
        operation({ signal: controller.signal, timeoutMs }),
        new Promise<never>((_, reject) => { timer = setTimeout(() => {
          controller.abort();
          reject(new Error("OMP lifecycle command timed out"));
        }, timeoutMs); }),
      ]);
    } catch (error) {
      this.disabled = true;
      throw error;
    } finally {
      clearTimeout(timer!);
      this.pending = undefined;
    }
  }

  private async show(): Promise<AgentPane> {
    return hostPane(await this.command(options => this.cli.agentShow({ ...options, target: this.host! })), this.host!);
  }

  private async verify(binding: Binding): Promise<AgentPane> {
    const pane = await this.show();
    if (!samePlace(pane, binding.pane) || !owns(pane, binding.id)) throw new Error("OMP owner changed");
    const child = pane.agent_session;
    if (binding.child === null) {
      if (child !== null) throw new Error("Unexpected OMP child");
    } else if (child?.resource !== binding.child || child.provider !== "omp" || child.native_id !== binding.id) {
      throw new Error("OMP child replaced");
    }
    return pane;
  }

  private async bind(id: string, generation: number, cold: boolean): Promise<void> {
    const pane = await this.show();
    if (!this.current(id, generation)) return;
    const initialize = needsDeclaration(pane, id, cold);
    const binding = this.createBinding(id, pane);
    this.binding = binding;
    if (initialize) await this.command(options => this.cli.agentSet(this.host!, declaration(id), options));
    await this.verify(binding);
    if (!this.current(id, generation)) return;
    if (this.declarationOnly) return this.closeChild();
    if (!binding.emitter.isOpen) await binding.emitter.bind(this.host!, id);
    if (!binding.emitter.isOpen) this.disabled = true;
  }

  private createBinding(id: string, pane: AgentPane): Binding {
    const emitter = new AgentSessionEmitter({
      agentSessionOpen: (target, options) => this.command(async execution => {
        const opened = await this.cli.agentSessionOpen(target, { ...options, ...execution });
        binding.child = opened.resource;
        return opened;
      }),
      agentEmit: (target, type, options) => this.command(execution => this.cli.agentEmit(target, type, { ...options, ...execution })),
      agentSessionClose: target => this.command(execution => this.cli.agentSessionClose(target, execution)),
    } satisfies Pick<Cli, "agentSessionOpen" | "agentEmit" | "agentSessionClose">, {
      provider: "omp", onError: () => { this.disabled = true; },
    });
    const binding: Binding = { id, pane, child: null, emitter };
    // Validate child identity BEFORE any declaration mutation.
    if (pane.agent_session !== null) {
      binding.emitter.adopt(this.host!, id, { schema_version: 1, parent: this.host!, ...pane.agent_session! });
      binding.child = pane.agent_session!.resource;
    }
    return binding;
  }

  private async closeChild(): Promise<void> {
    const binding = this.binding;
    if (!binding) return;
    await this.verify(binding);
    await binding.emitter.finish();
    binding.child = null;
    await this.verify(binding);
  }

  private async rotateDeclaration(id: string): Promise<void> {
    const binding = this.binding;
    if (!binding) return;
    await this.closeChild();
    // Preserve the proven declaration across rotation; clearing it would open
    // a window for the detector to publish an unowned record before admission.
    await this.command(options => this.cli.agentSet(this.host!, declaration(id), options));
    binding.id = id;
  }

  private async retire(): Promise<void> {
    if (!this.binding) return;
    await this.closeChild();
    await this.command(options => this.cli.agentClear(this.host!, options));
    this.binding = undefined;
  }
}

export function registerOmpLifecycle(pi: ExtensionAPI, cli: Cli, host?: string): OmpLifecycle {
  const lifecycle = new OmpLifecycle(cli, host);
  type Token = NonNullable<ReturnType<OmpLifecycle["token"]>>;
  let phase: "idle" | "prepared" | "active" | "continuing" | "fallback" = "idle";
  let token: Token | undefined;
  let firstNavigation = true;
  const tools = new Map<string, string>();
  const endedTools = new Set<string>();
  const approvals = new Map<string, string>();
  const resolvedApprovals = new Set<string>();

  function fallback(): Promise<void> {
    phase = "fallback";
    token = undefined;
    tools.clear();
    approvals.clear();
    return lifecycle.fallback();
  }

  function emit(type: AgentEventType, data?: Readonly<Record<string, unknown>>): Promise<void> | undefined {
    if (token) return lifecycle.emit(token.id, token.generation, type, data);
  }

  /** The host's own busy flag; absent means unproven, which keeps the fallback. */
  function streaming(ctx: ExtensionContext): boolean {
    return typeof ctx.isIdle === "function" && ctx.isIdle() === false;
  }

  function activeApproval(event: { sessionId: string; toolCallId: string; toolName: string }): boolean {
    return token !== undefined && event.sessionId === token.id &&
      event.toolCallId.length > 0 && event.toolName.length > 0;
  }

  const navigate = async (event: { type: string }, ctx: ExtensionContext) => {
    const id = ctx.sessionManager.getSessionId();
    const cold = firstNavigation && event.type === "session_start";
    firstNavigation = false;
    const sameId = lifecycle.token()?.id === id;
    if (sameId && event.type === "session_tree") return;
    if (phase !== "idle" && phase !== "fallback") await fallback();
    token = undefined;
    tools.clear();
    endedTools.clear();
    approvals.clear();
    resolvedApprovals.clear();
    await lifecycle.navigate(id, cold);
  };
  pi.on("session_start", navigate);
  pi.on("session_switch", navigate);
  pi.on("session_branch", navigate);
  pi.on("session_tree", navigate);

  // This is a payload-free causal guard, NOT prompt reporting. In 18.8.6
  // AgentSession.prompt awaits emitBeforeAgentStart before agent.prompt. Generic
  // agent/tool notifications are concurrent; their receipt alone is not a guard.
  // 18.x also runs this hook for queued steering/follow-up deliveries the running
  // loop absorbs (no agent_start of their own), and may repeat it for one prompt.
  // While the session is streaming either is a no-op: a genuinely overlapping loop
  // must still deliver its own agent_start, which falls back below. An idle
  // session cannot be absorbing a delivery, so a guard behind an unreceived end
  // still falls back (docs/consumers/omp.md, "Host-bound lifecycle").
  pi.on("before_agent_start", (_event, ctx) => {
    if (phase === "fallback") return;
    if ((phase === "prepared" || phase === "active") && streaming(ctx)) return;
    if (phase !== "idle" && phase !== "continuing") return fallback();
    token = lifecycle.token();
    phase = "prepared";
    tools.clear();
    endedTools.clear();
  });
  pi.on("agent_start", () => {
    if (phase === "fallback") return;
    if (phase !== "prepared" && phase !== "continuing") return fallback();
    phase = "active";
    if (approvals.size === 0) return emit("prompt");
  });
  pi.on("tool_execution_start", event => {
    if (!token || endedTools.has(event.toolCallId) || tools.has(event.toolCallId)) return;
    tools.set(event.toolCallId, event.toolName);
    // tool_start asserts working: never clear another concurrent approval.
    if (approvals.size === 0) return emit("tool_start", { tool_name: event.toolName, tool_use_id: event.toolCallId });
  });
  pi.on("tool_execution_end", event => {
    if (!token || endedTools.has(event.toolCallId)) return;
    const name = tools.get(event.toolCallId);
    if (name !== undefined && name !== event.toolName) return fallback();
    endedTools.add(event.toolCallId);
    tools.delete(event.toolCallId);
    return emit("tool_end", { tool_name: event.toolName, tool_use_id: event.toolCallId, ok: !event.isError });
  });
  pi.on("tool_approval_requested", event => {
    if (!activeApproval(event) || resolvedApprovals.has(event.toolCallId)) return;
    if (approvals.has(event.toolCallId)) return;
    approvals.set(event.toolCallId, event.toolName);
    return emit("notification", { kind: "permission" });
  });
  pi.on("tool_approval_resolved", event => {
    if (!activeApproval(event) || approvals.get(event.toolCallId) !== event.toolName) return;
    approvals.delete(event.toolCallId);
    resolvedApprovals.add(event.toolCallId);
    // The wrapper resumes after either approval or denial; neither is completion.
    if (approvals.size === 0) return emit("state", { state: "working" });
  });
  pi.on("agent_end", event => {
    if (!token) return;
    if (approvals.size > 0) return fallback();
    // The wrapper awaits approval delivery inline before completing its tool,
    // so approval events causally precede the end and completed approval IDs
    // can be forgotten here (also for automatic continuation).
    resolvedApprovals.clear();
    // Generic tool deliveries are NOT fenced by the end. 17.x held agent_end
    // behind them with a FIFO subscriber gate; 18.8.6 dispatches each agent
    // event fire-and-forget and settles agent_end on its own path, so a tool
    // event an earlier extension holds can arrive after this. After a terminal
    // end the token is gone and such an event is dropped; after a continuing
    // end it can only re-assert working, which the next loop is. The lifecycle
    // smoke's delayed-tool case pins both the ordering and that tolerance.
    if (event.willContinue === true) {
      phase = "continuing";
      tools.clear();
      endedTools.clear();
      return;
    }
    const result = emit("stop");
    phase = "idle";
    token = undefined;
    return result;
  });
  pi.on("session_shutdown", () => {
    token = undefined;
    approvals.clear();
    return lifecycle.shutdown();
  });
  return lifecycle;
}
