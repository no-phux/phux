import { randomUUID } from "node:crypto";

import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { clampThinkingLevel, type AssistantMessage, type Message, type ModelThinkingLevel } from "@earendil-works/pi-ai";
import type { ModelRuntime } from "@earendil-works/pi-coding-agent";
import { defineDoc, type AgentEvent, type AgentState, type Conversation, type Harness } from "@earendil-works/pi-durable";
import { contentText, type MessageTranscript } from "@phux/integration-runtime/message-transcript";
import { transcriptData, type TranscriptEntry } from "@phux/integration-runtime/transcript";

import { LoginBroker, type LoginRecord, type LoginStep } from "./auth.js";
import { splitModel, type ChiefConfig, type ChiefPaths } from "./config.js";
import { chiefFinding, investigationPrompt, type ErrorIntake, type Finding } from "./diagnostics.js";
import { HELP, parseInput, type Command } from "./envelope.js";
import { applyChanges } from "./partial.js";
import { pages, type ChiefStream } from "./phux-stream.js";

const context = BACKGROUND_CONTEXT;
export const CHIEF_VERSION = "0.1.0";
const MAX_REPLY_BYTES = 8 * 1024;
const LOCKED_PREFIX = "[sent while the phone was locked] ";

interface HeldItem {
  id: string;
  text: string;
  at: number;
  [key: string]: string | number;
}

/** Messages the phone sent before Face ID unlock, kept out of the model until `unlock`. */
export const HeldDoc = defineDoc<{ items: HeldItem[] }>({
  kind: "phux.chief.held",
  version: 1,
  scope: "conversation",
  history: "latest",
  fork: "initial",
  initial: () => ({ items: [] }),
  checkpointWhen: () => true,
});

interface ChiefParts {
  readonly config: ChiefConfig;
  readonly paths: ChiefPaths;
  readonly harness: Harness;
  readonly root: Conversation;
  readonly models: ModelRuntime;
  readonly stream: ChiefStream;
  readonly transcript: MessageTranscript;
  readonly print: (line: string) => void;
  readonly log: (line: string) => void;
  readonly host: string;
  readonly hostPane: string | undefined;
  readonly intake: ErrorIntake;
  readonly pendingFindings: Finding[];
}

/** Commands from the pane and events from the Harness, one at a time. */
export class Chief {
  private queue: Promise<void> = Promise.resolve();
  private partial: AssistantMessage | undefined;
  private lastReply = "";
  private busy = false;
  private readonly requests = new Map<number, string>();
  private readonly toolFailures = new Map<string, number>();
  private readonly login: LoginBroker;
  private notes = 0;

  constructor(private readonly parts: ChiefParts) {
    this.login = new LoginBroker(parts.models, (record, step) => this.onLoginStep(record, step));
  }

  resumedRun(): void {
    this.busy = true;
  }

  submitText(text: string): Promise<void> {
    const parsed = parseInput(text);
    if (parsed === null) return Promise.resolve();
    if (!parsed.ok) {
      this.parts.print(`! ${parsed.error}`);
      return this.parts.stream.chief("ack", { id: parsed.id ?? null, status: "rejected", detail: parsed.error });
    }
    return this.handle(parsed.command);
  }

  handle(command: Command): Promise<void> {
    const run = async () => {
      try {
        await this.dispatch(command);
      } catch (error) {
        const detail = error instanceof Error ? error.message : String(error);
        this.parts.print(`! ${detail}`);
        await this.parts.stream.chief("ack", { id: command.id ?? null, status: "rejected", detail });
      }
    };
    this.queue = this.queue.then(run);
    return this.queue;
  }

  private async dispatch(command: Command): Promise<void> {
    const { root, stream, config } = this.parts;
    const ack = (status: string, extra: Record<string, unknown> = {}) => stream.chief("ack", { id: command.id ?? null, status, ...extra });
    switch (command.type) {
      case "message": {
        if (command.locked && !config.yolo) return this.hold(command.id ?? randomUUID(), command.text, ack);
        const text = command.locked ? `${LOCKED_PREFIX}${command.text}` : command.text;
        await this.admit(command.id ?? randomUUID(), text, ack);
        return;
      }
      case "unlock": {
        const held = await this.held();
        for (const item of held) await this.admit(item.id, `${LOCKED_PREFIX}${item.text}`, () => Promise.resolve());
        await root.commit(async (tx) => {
          const doc = await tx.doc(HeldDoc, root.id);
          doc.items.splice(0, doc.items.length);
        }, context);
        if (held.length > 0) await this.note(`Unlocked: released ${held.length} held message${held.length === 1 ? "" : "s"}.`);
        await ack("done", { released: held.map((item) => item.id) });
        await this.publishStatus();
        return;
      }
      case "abort":
        await root.abort(context);
        await ack("done");
        return;
      case "status":
        await this.publishStatus();
        await ack("done");
        return;
      case "help":
        this.parts.print(HELP);
        await ack("done", { help: HELP });
        return;
      case "models":
        await this.publishModels(command.query);
        await ack("done");
        return;
      case "model": {
        const { provider, modelId } = splitModel(command.model);
        const model = this.parts.models.getModel(provider, modelId);
        if (model === undefined) throw new Error(`unknown model ${command.model}; see /models`);
        if (!this.parts.models.hasConfiguredAuth(provider)) throw new Error(`${provider} has no credentials; /login ${provider} first`);
        const agent = await this.agentState();
        const requested = (command.thinking ?? agent.thinkingLevel ?? config.thinking) as ModelThinkingLevel;
        const thinkingLevel = clampThinkingLevel(model, requested);
        await root.configure({ model: { provider, modelId }, thinkingLevel }, context);
        this.parts.print(`model → ${provider}/${modelId} (thinking ${thinkingLevel})`);
        await ack("done", { model: `${provider}/${modelId}`, thinking: thinkingLevel });
        await this.publishStatus();
        return;
      }
      case "thinking": {
        const agent = await this.agentState();
        const ref = agent.model;
        const model = ref === undefined ? undefined : this.parts.models.getModel(ref.provider, ref.modelId);
        if (model === undefined) throw new Error("no current model");
        const thinkingLevel = clampThinkingLevel(model, command.level as ModelThinkingLevel);
        await root.configure({ thinkingLevel }, context);
        await ack("done", { thinking: thinkingLevel });
        await this.publishStatus();
        return;
      }
      case "login": {
        const flow = this.login.start(command.provider, command.method);
        const current = this.login.current;
        void flow.catch((error: unknown) => this.parts.print(`! login: ${error instanceof Error ? error.message : String(error)}`));
        await ack("started", current === undefined ? {} : { login: current.login, provider: current.provider });
        // A synchronous refusal (unknown provider) surfaces as the rejection.
        if (current === undefined) await flow;
        return;
      }
      case "login_answer":
        await ack(this.login.answer(command.login, command.value) ? "done" : "rejected", this.login.current === undefined ? { detail: "no login is waiting for an answer" } : {});
        return;
      case "login_cancel":
        await ack(this.login.cancel(command.login) ? "done" : "rejected");
        return;
      case "logout":
        await this.parts.models.logout(command.provider);
        this.parts.print(`logged out of ${command.provider}`);
        await ack("done");
        await this.publishStatus();
        return;
      case "compact":
        await root.compact(command.instructions, context);
        await ack("started");
        return;
    }
  }

  /** Durably admit one turn. `requestId` makes a resend after a lost ack a no-op. */
  private async admit(id: string, text: string, ack: (status: string, extra?: Record<string, unknown>) => Promise<void>): Promise<void> {
    const submission = await this.parts.root.submit({ type: "input", content: text, requestId: id, whenBusy: "followUp" }, context);
    this.requests.set(submission.id, id);
    const record = await submission.status(context);
    await ack(record.status === "queued" || record.status === "placed" ? "admitted" : record.status, { submission: submission.id, state: record.status });
  }

  private async hold(id: string, text: string, ack: (status: string, extra?: Record<string, unknown>) => Promise<void>): Promise<void> {
    const { root } = this.parts;
    const duplicate = await root.commit(async (tx) => {
      const doc = await tx.doc(HeldDoc, root.id);
      if (doc.items.some((item) => item.id === id)) return true;
      doc.items.push({ id, text, at: Date.now() });
      return false;
    }, context);
    if (!duplicate) {
      this.parts.print(`(held until unlock) ${text}`);
      await this.parts.stream.emit("provider_raw", transcriptData("pi", userEntry(`held-${id}`, text)));
      await this.note("Held until you unlock the phone: tools do not run for messages sent before Face ID. Unlock to release it.");
    }
    await ack(duplicate ? "duplicate" : "held");
    await this.publishStatus();
  }

  private async held(): Promise<HeldItem[]> {
    return (await this.parts.harness.snapshot(HeldDoc, this.parts.root.id, context))?.items ?? [];
  }

  private async agentState(): Promise<AgentState> {
    const agent = await this.parts.root.agent(context);
    return { ...(agent.model === undefined ? {} : { model: agent.model }), thinkingLevel: agent.thinkingLevel };
  }

  /** A visible system line in the phone thread and the pane, never seen by the model. */
  private async note(text: string): Promise<void> {
    this.notes += 1;
    this.parts.print(`· ${text}`);
    await this.parts.stream.emit("provider_raw", transcriptData("pi", { id: `chief-note-${Date.now()}-${this.notes}`, role: "system", text, truncated: false, final: true }));
  }

  async publishStatus(): Promise<void> {
    const agent = await this.agentState();
    const model = agent.model === undefined ? undefined : this.parts.models.getModel(agent.model.provider, agent.model.modelId);
    const held = await this.held();
    const providers = this.parts.models.getProviders().map((candidate) => {
      const status = this.parts.models.getProviderAuthStatus(candidate.id);
      return {
        id: candidate.id,
        name: candidate.name,
        configured: status.configured,
        ...(status.label === undefined ? {} : { source: status.label }),
        oauth: candidate.auth.oauth !== undefined,
        api_key: candidate.auth.apiKey?.login !== undefined,
      };
    });
    const status = {
      version: CHIEF_VERSION,
      host: this.parts.host,
      pane: this.parts.hostPane ?? null,
      model: agent.model === undefined ? null : { provider: agent.model.provider, id: agent.model.modelId, name: model?.name ?? agent.model.modelId },
      thinking: agent.thinkingLevel ?? null,
      yolo: this.parts.config.yolo,
      busy: this.busy,
      held: held.map((item) => ({ id: item.id, preview: item.text.slice(0, 120), at: item.at })),
      login: this.login.current ?? null,
      self_heal: this.parts.config.selfHeal,
    };
    // Providers ride their own pages so a long catalog never pushes status over the record limit.
    await this.parts.stream.chief("status", status);
    const chunks = pages(providers);
    for (const [index, chunk] of chunks.entries()) await this.parts.stream.chief("providers", { page: index, pages: chunks.length, providers: chunk });
  }

  private async publishModels(query: string | undefined): Promise<void> {
    const needle = query?.toLowerCase();
    const available = await this.parts.models.getAvailable();
    const models = available
      .filter((model) => needle === undefined || `${model.provider}/${model.id} ${model.name}`.toLowerCase().includes(needle))
      .map((model) => ({ provider: model.provider, id: model.id, name: model.name, context: model.contextWindow, reasoning: model.reasoning }));
    const chunks = pages(models);
    for (const [index, chunk] of chunks.entries()) await this.parts.stream.chief("models", { page: index, pages: chunks.length, query: query ?? null, models: chunk });
    this.parts.print(models.slice(0, 60).map((model) => `  ${model.provider}/${model.id}  ${model.name}`).join("\n") + (models.length > 60 ? `\n  … ${models.length - 60} more` : ""));
  }

  private onLoginStep(record: LoginRecord, step: LoginStep): void {
    const describe = (): string => {
      switch (step.step) {
        case "url": return `open ${step.url}${step.instructions ? ` — ${step.instructions}` : ""}`;
        case "device_code": return `enter code ${step.user_code} at ${step.verification_uri}`;
        case "prompt": return `${step.prompt.message}${step.prompt.kind === "manual_code" || step.prompt.kind === "text" || step.prompt.kind === "secret" ? `  (answer: /code ${record.login} VALUE)` : ""}`;
        case "info": case "progress": return step.message;
        case "failed": return `failed: ${step.error}`;
        default: return step.step;
      }
    };
    this.parts.print(`login ${record.provider}: ${describe()}`);
    void this.parts.stream.chief("login", { ...record, ...step });
    if (step.step === "done" || step.step === "failed" || step.step === "cancelled") void this.publishStatus();
  }

  async onEvent(event: AgentEvent): Promise<void> {
    const { stream, transcript, print } = this.parts;
    const emitAll = async (records: readonly Readonly<Record<string, unknown>>[]) => {
      for (const data of records) await stream.emit("provider_raw", data);
    };
    switch (event.type) {
      case "run_start":
        this.busy = true;
        this.lastReply = "";
        await stream.emit("prompt");
        await stream.state("working");
        return;
      case "message_start":
        if (event.message.role === "assistant") this.partial = structuredClone(event.message);
        return;
      case "message_update": {
        if (this.partial === undefined) return;
        const kind = applyChanges(this.partial, event.changes);
        if (kind !== undefined) await emitAll(transcript.messageUpdate(this.partial, kind));
        return;
      }
      case "message_end": {
        const message = event.entry.model?.[0] as Message | undefined;
        if (message === undefined || message.role === "toolResult") return;
        if (message.role === "assistant") {
          this.partial = undefined;
          const text = contentText(message.content).trim();
          if (text.length > 0) {
            this.lastReply = text;
            print(`\r\nchief: ${text}\r\n`);
          }
          if (message.stopReason === "error" && message.errorMessage) print(`! ${message.errorMessage}`);
        } else if (message.role === "user") {
          print(`you: ${contentText(message.content).trim()}`);
        } else {
          return;
        }
        await emitAll(transcript.messageEnd(message));
        return;
      }
      case "tool_execution_start":
        if (event.parentToolCallId !== undefined) return;
        print(`  → ${event.toolName} ${summarize(event.args)}`);
        await stream.emit("tool_start", { tool_name: event.toolName, tool_use_id: event.toolCallId });
        await emitAll(transcript.toolStart({ toolCallId: event.toolCallId, toolName: event.toolName, args: event.args }));
        return;
      case "tool_execution_end": {
        if (event.parentToolCallId !== undefined) return;
        const result = event.entry?.model?.[0] as (Message & { isError?: boolean }) | undefined;
        const isError = result === undefined || result.isError === true;
        print(`  ${isError ? "✗" : "✓"} ${event.toolName}`);
        await stream.emit("tool_end", { tool_name: event.toolName, tool_use_id: event.toolCallId, ok: !isError });
        await emitAll(transcript.toolEnd({ toolCallId: event.toolCallId, toolName: event.toolName, isError, result }));
        this.trackToolFailure(event.toolName, isError, result === undefined ? "no result" : contentText(result.content));
        return;
      }
      case "run_end": {
        this.busy = false;
        const reply = this.lastReply.length > MAX_REPLY_BYTES ? `${this.lastReply.slice(0, MAX_REPLY_BYTES)}…` : this.lastReply;
        await stream.emit("stop", {
          turn_id: `run-${event.inputs[0] ?? Date.now()}`,
          ...(this.parts.config.pushReplyText && reply.length > 0 ? { reply, reply_title: "Chief" } : {}),
        });
        await stream.state("done");
        await this.publishStatus();
        return;
      }
      case "submission": {
        const id = this.requests.get(event.record.id);
        if (id === undefined) return;
        if (event.record.status === "done" || event.record.status === "unanswered") {
          this.requests.delete(event.record.id);
          await stream.chief("ack", {
            id,
            status: event.record.status,
            submission: event.record.id,
            ...(event.record.status === "unanswered" ? { detail: event.record.reason ?? null } : {}),
          });
        }
        return;
      }
      case "auto_retry_start":
        await this.note(`Provider error, retrying (attempt ${event.attempt}): ${event.errorMessage.slice(0, 200)}`);
        return;
      case "task_failed":
        this.parts.log(`task ${event.kind} failed: ${event.message}`);
        this.parts.pendingFindings.push(...this.parts.intake.admit([chiefFinding(`task ${event.kind} failed: ${event.message}`)]));
        return;
      case "compaction_end":
        if (event.error !== undefined) await this.note(`Compaction failed: ${event.error}`);
        return;
      case "agent_changed":
        await this.publishStatus();
        return;
      default:
        return;
    }
  }

  private trackToolFailure(tool: string, failed: boolean, detail: string): void {
    if (!failed) {
      this.toolFailures.delete(tool);
      return;
    }
    const count = (this.toolFailures.get(tool) ?? 0) + 1;
    this.toolFailures.set(tool, count);
    if (count === 3) {
      this.parts.pendingFindings.push(...this.parts.intake.admit([chiefFinding(`tool ${tool} failed 3 times in a row: ${detail.slice(0, 400)}`)]));
    }
  }

  /** Scan error sources and hand anything new to the chief, within the self-heal policy. */
  async runIntake(): Promise<void> {
    const { config, intake, pendingFindings } = this.parts;
    if (config.selfHeal === "off") return;
    try {
      pendingFindings.push(...await intake.scan());
    } catch (error) {
      this.parts.log(`intake: ${error instanceof Error ? error.message : String(error)}`);
    }
    if (pendingFindings.length === 0) return;
    const findings = pendingFindings.splice(0, pendingFindings.length);
    await this.parts.stream.chief("alert", { findings: findings.map(({ fingerprint, source, title }) => ({ fingerprint, source, title })) });
    if (config.selfHeal === "auto" && intake.claimInvestigation(config.selfHealCooldownMinutes)) {
      await this.handle({ type: "message", id: `intake-${findings[0]!.fingerprint.replace(/[^A-Za-z0-9._:-]/g, "-").slice(0, 100)}`, text: investigationPrompt(findings, config.ownedRepos), locked: false });
      return;
    }
    await this.note(`Error intake: ${findings.map((finding) => finding.title).join("; ").slice(0, 600)}`);
    if (config.selfHeal === "auto") pendingFindings.push(...findings);
  }
}

function userEntry(id: string, text: string): TranscriptEntry {
  return { id, role: "user", text, truncated: false, final: true };
}

function summarize(args: unknown): string {
  const text = JSON.stringify(args ?? {});
  return text.length > 120 ? `${text.slice(0, 117)}…` : text;
}
