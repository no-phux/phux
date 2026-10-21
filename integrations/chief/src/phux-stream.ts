import { readFileSync, writeFileSync } from "node:fs";

import { AgentSessionEmitter, type PhuxCli } from "@phux/integration-runtime/adapter";
import type { AgentEventType } from "@phux/integration-runtime/schemas";
import { MAX_TRANSCRIPT_DATA_BYTES, utf8Length } from "@phux/integration-runtime/transcript";

/** Status and control records the chief publishes beside its transcript (docs/chief.md). */
export const CHIEF_SCHEMA = "phux.chief/v1";
/** The pane tag and agent-record name every client resolves the chief by. */
export const CHIEF_ROLE = "chief";
const TIMEOUT_MS = 5_000;

export type StreamCli = Pick<PhuxCli, "agentSet" | "agentClear" | "agentSessionOpen" | "agentEmit" | "agentSessionClose" | "tag">;

/**
 * The chief's presence on its own pane: the `chief` declaration and tag,
 * and the AgentSession stream every client renders. Writes are serialized
 * and best effort: a phux hiccup must never stop the agent.
 */
export class ChiefStream {
  private readonly emitter: AgentSessionEmitter;
  private tail: Promise<void> = Promise.resolve();

  /**
   * `sessionFile` remembers the AgentSession this chief opened, so the next
   * process can close it when a crash left it open: two live sessions on one
   * pane make the pane's stream ambiguous to every client.
   */
  constructor(
    private readonly cli: StreamCli,
    private readonly pane: string | undefined,
    private readonly nativeId: string,
    private readonly onError: (error: unknown) => void,
    private readonly sessionFile?: string,
  ) {
    // Methods live on the CLI's prototype, so the wrapper binds them rather than spreading.
    const recording: StreamCli = {
      agentSet: cli.agentSet.bind(cli),
      agentClear: cli.agentClear.bind(cli),
      agentEmit: cli.agentEmit.bind(cli),
      agentSessionClose: cli.agentSessionClose.bind(cli),
      tag: cli.tag.bind(cli),
      agentSessionOpen: async (target, options) => {
        const opened = await cli.agentSessionOpen(target, options);
        this.remember({ pane: target, resource: opened.resource });
        return opened;
      },
    };
    this.emitter = new AgentSessionEmitter(recording, { provider: "pi", onError });
  }

  get bound(): boolean {
    return this.emitter.isOpen;
  }

  /** Declare the pane, tag it, and open its AgentSession. */
  bind(): Promise<void> {
    const pane = this.pane;
    if (pane === undefined) return Promise.resolve();
    return this.enqueue(async () => {
      const options = { timeoutMs: TIMEOUT_MS };
      await this.cli.agentSet(pane, this.record("idle"), options);
      await this.cli.tag("add", pane, [CHIEF_ROLE], options);
      await this.closeStale(pane);
      await this.emitter.bind(pane, this.nativeId, options);
    });
  }

  /**
   * Declare the pane's state. The server's detector identifies an occupant
   * by its process, and the chief runs as `node`, so the AgentSession's own
   * evidence never reaches the pane; a declared state is authoritative.
   */
  state(state: "idle" | "working" | "blocked" | "done"): Promise<void> {
    const pane = this.pane;
    if (pane === undefined) return Promise.resolve();
    return this.enqueue(async () => {
      await this.cli.agentSet(pane, this.record(state), { timeoutMs: TIMEOUT_MS });
    });
  }

  private record(state: "idle" | "working" | "blocked" | "done") {
    return { name: CHIEF_ROLE, kind: "pi", session: `chief:${this.nativeId}`, state };
  }

  emit(type: AgentEventType, data?: Readonly<Record<string, unknown>>): Promise<void> {
    return this.enqueue(() => this.emitter.emit(type, data, { timeoutMs: TIMEOUT_MS }));
  }

  /** One `phux.chief/v1` record; oversized payloads are refused here, not by the server. */
  chief(event: string, data: Readonly<Record<string, unknown>> = {}): Promise<void> {
    const record = { schema: CHIEF_SCHEMA, event, ...data };
    if (utf8Length(JSON.stringify(record)) > MAX_TRANSCRIPT_DATA_BYTES) {
      this.onError(new Error(`chief ${event} record exceeds ${MAX_TRANSCRIPT_DATA_BYTES} bytes; dropped`));
      return Promise.resolve();
    }
    return this.emit("provider_raw", record);
  }

  close(): Promise<void> {
    const pane = this.pane;
    return this.enqueue(async () => {
      await this.emitter.finish({ timeoutMs: TIMEOUT_MS });
      if (pane !== undefined) await this.cli.agentClear(pane, { timeoutMs: TIMEOUT_MS });
    });
  }

  settled(): Promise<void> {
    return this.tail;
  }

  /** Close the session a previous process recorded on this same pane; a gone one is fine. */
  private async closeStale(pane: string): Promise<void> {
    const previous = this.recalled();
    if (previous === undefined || previous.pane !== pane) return;
    const options = { timeoutMs: TIMEOUT_MS };
    await this.cli.agentEmit(previous.resource, "session_end", options).catch(() => undefined);
    await this.cli.agentSessionClose(previous.resource, options).catch(() => undefined);
  }

  private recalled(): { pane: string; resource: string } | undefined {
    if (this.sessionFile === undefined) return undefined;
    try {
      const value = JSON.parse(readFileSync(this.sessionFile, "utf8")) as { pane?: unknown; resource?: unknown };
      return typeof value.pane === "string" && typeof value.resource === "string" ? { pane: value.pane, resource: value.resource } : undefined;
    } catch {
      return undefined;
    }
  }

  private remember(value: { pane: string; resource: string }): void {
    if (this.sessionFile === undefined) return;
    try {
      writeFileSync(this.sessionFile, JSON.stringify(value));
    } catch (error) {
      this.onError(error);
    }
  }

  private enqueue(work: () => Promise<void>): Promise<void> {
    this.tail = this.tail.then(work).catch((error: unknown) => this.onError(error));
    return this.tail;
  }
}

/** Split `items` into pages whose JSON stays under the record budget. */
export function pages<T>(items: readonly T[], budget = MAX_TRANSCRIPT_DATA_BYTES - 512): T[][] {
  const out: T[][] = [];
  let page: T[] = [];
  let size = 2;
  for (const item of items) {
    const bytes = utf8Length(JSON.stringify(item)) + 1;
    if (page.length > 0 && size + bytes > budget) {
      out.push(page);
      page = [];
      size = 2;
    }
    page.push(item);
    size += bytes;
  }
  if (page.length > 0 || out.length === 0) out.push(page);
  return out;
}
