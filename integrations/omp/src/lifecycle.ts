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

/** Host-specific serialized writer. An uncertain operation disables this instance;
 * no mutation is retried, including after navigation. Tools are independent. */
export class OmpLifecycle {
  private tail: Promise<void> = Promise.resolve();
  private generation = 0;
  private id: string | undefined;
  private binding: Binding | undefined;
  private disabled = false;
  private stopped = false;
  private deadline = Infinity;
  private pending: AbortController | undefined;

  constructor(private readonly cli: Cli, private readonly host?: string, private readonly timeoutMs = 250) {}

  navigate(id: string): Promise<void> {
    if (this.stopped || this.disabled || !this.host) return this.tail;
    if (this.id === id) return this.tail;
    this.id = id;
    const generation = ++this.generation;
    return this.enqueue(async () => {
      if (!this.current(id, generation)) return;
      if (this.binding?.id === id) return;
      await this.retire();
      if (this.current(id, generation)) await this.bind(id, generation);
    });
  }

  emit(id: string, generation: number, type: AgentEventType, data?: Readonly<Record<string, unknown>>): Promise<void> {
    return this.enqueue(async () => {
      if (!this.current(id, generation)) return;
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

  private async bind(id: string, generation: number): Promise<void> {
    const pane = await this.show();
    if (!this.current(id, generation)) return;
    const declared = pane.sources.some(source => source.kind === "agent_record");
    if (declared && !owns(pane, id)) throw new Error("Foreign OMP declaration");
    if (pane.agent_session !== null && !owns(pane, id)) throw new Error("Unowned OMP child");
    const binding = this.createBinding(id, pane);
    this.binding = binding;
    if (!declared) await this.command(options => this.cli.agentSet(this.host!, declaration(id), options));
    await this.verify(binding);
    if (!this.current(id, generation)) return;
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

  private async retire(): Promise<void> {
    const binding = this.binding;
    if (!binding) return;
    await this.verify(binding);
    await binding.emitter.finish();
    // Clear only after exact child close AND a fresh matching declaration proof.
    binding.child = null;
    await this.verify(binding);
    await this.command(options => this.cli.agentClear(this.host!, options));
    this.binding = undefined;
  }
}

export function registerOmpLifecycle(pi: ExtensionAPI, cli: Cli, host?: string): OmpLifecycle {
  const lifecycle = new OmpLifecycle(cli, host);
  let loop: ReturnType<OmpLifecycle["token"]>;
  let ambiguousLoop = false;
  const tools = new Map<string, NonNullable<typeof loop>>();
  const navigate = (event: { type: string }, ctx: ExtensionContext) => {
    const id = ctx.sessionManager.getSessionId();
    if (lifecycle.token()?.id === id) return lifecycle.navigate(id);
    // OMP 17.1.2 switch/new disconnect and abort/drain post-prompt tasks before
    // publishing the new ID (agent-session.ts switchSession/newSession/abort).
    // branch/tree do not offer that barrier, nor do activity events carry IDs.
    // If navigated during a loop, fail closed for subsequent activity rather
    // than attributing a late aggregate end to a newly started loop.
    if (loop && (event.type === "session_branch" || event.type === "session_tree")) ambiguousLoop = true;
    loop = undefined;
    tools.clear();
    return lifecycle.navigate(id);
  };
  pi.on("session_start", navigate);
  pi.on("session_switch", navigate);
  pi.on("session_branch", navigate);
  pi.on("session_tree", navigate);
  pi.on("agent_start", () => {
    if (ambiguousLoop) return;
    loop = lifecycle.token();
    if (loop) return lifecycle.emit(loop.id, loop.generation, "prompt");
  });
  pi.on("tool_execution_start", event => {
    if (!loop) return;
    tools.set(event.toolCallId, loop);
    return lifecycle.emit(loop.id, loop.generation, "tool_start", { tool_name: event.toolName, tool_use_id: event.toolCallId });
  });
  pi.on("tool_execution_end", event => {
    const token = tools.get(event.toolCallId);
    tools.delete(event.toolCallId);
    if (token) return lifecycle.emit(token.id, token.generation, "tool_end", {
      tool_name: event.toolName, tool_use_id: event.toolCallId, ok: !event.isError,
    });
  });
  pi.on("agent_end", event => {
    if (event.willContinue === true || !loop) return;
    const token = loop;
    loop = undefined;
    return lifecycle.emit(token.id, token.generation, "stop");
  });
  pi.on("session_shutdown", () => {
    loop = undefined;
    tools.clear();
    return lifecycle.shutdown();
  });
  return lifecycle;
}
