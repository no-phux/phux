import { execFile, spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { isAbsolute } from 'node:path';
import { StringDecoder } from 'node:string_decoder';
import { promisify } from 'node:util';
import type { Ajv, ValidateFunction } from 'ajv';
import { keepHead, keepTail, MAX_TOOL_OUTPUT_BYTES, summarizeArgs, transcriptData, transcriptMode } from '../../runtime/src/transcript.js';
import type { AdapterContext, Json, NativeAdapter, NativeQuestion, TranscriptEntry } from './types.js';
import type { InitializeParams } from './codex-protocol/generated/InitializeParams.js';
import type { InitializeResponse } from './codex-protocol/generated/InitializeResponse.js';
import type { RequestId } from './codex-protocol/generated/RequestId.js';
import type { ServerRequest } from './codex-protocol/generated/ServerRequest.js';
import type { ServerNotification } from './codex-protocol/generated/ServerNotification.js';
import type { ThreadItem } from './codex-protocol/generated/v2/ThreadItem.js';
import type { Thread } from './codex-protocol/generated/v2/Thread.js';
import type { ThreadStartParams } from './codex-protocol/generated/v2/ThreadStartParams.js';
import type { ThreadStartResponse } from './codex-protocol/generated/v2/ThreadStartResponse.js';
import type { TurnStartParams } from './codex-protocol/generated/v2/TurnStartParams.js';
import type { TurnStartResponse } from './codex-protocol/generated/v2/TurnStartResponse.js';
import type { Turn } from './codex-protocol/generated/v2/Turn.js';
import type { TurnInterruptParams } from './codex-protocol/generated/v2/TurnInterruptParams.js';
import type { TurnInterruptResponse } from './codex-protocol/generated/v2/TurnInterruptResponse.js';
import type { CommandExecutionApprovalDecision } from './codex-protocol/generated/v2/CommandExecutionApprovalDecision.js';
import type { ThreadResumeParams } from './codex-protocol/generated/v2/ThreadResumeParams.js';
import type { ThreadResumeResponse } from './codex-protocol/generated/v2/ThreadResumeResponse.js';
import type { FileChangeApprovalDecision } from './codex-protocol/generated/v2/FileChangeApprovalDecision.js';
import type { PermissionsRequestApprovalResponse } from './codex-protocol/generated/v2/PermissionsRequestApprovalResponse.js';
import type { ToolRequestUserInputResponse } from './codex-protocol/generated/v2/ToolRequestUserInputResponse.js';
import type { McpServerElicitationRequestResponse } from './codex-protocol/generated/v2/McpServerElicitationRequestResponse.js';
import requestSchema from './codex-protocol/schema/ServerRequest.json';
import notificationSchema from './codex-protocol/schema/ServerNotification.json';
import initializeSchema from './codex-protocol/schema/v1/InitializeResponse.json';
import threadSchema from './codex-protocol/schema/v2/ThreadStartResponse.json';
import resumeSchema from './codex-protocol/schema/v2/ThreadResumeResponse.json';
import turnSchema from './codex-protocol/schema/v2/TurnStartResponse.json';
import interruptSchema from './codex-protocol/schema/v2/TurnInterruptResponse.json';
import permissionsSchema from './codex-protocol/schema/PermissionsRequestApprovalResponse.json';
import elicitationSchema from './codex-protocol/schema/McpServerElicitationRequestResponse.json';
import protocolSource from './codex-protocol/source.json';

// Official Apache-2.0 protocol: openai/codex rust-v0.162.1,
// commit 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d. No CLI/SDK binary is redistributed.
// Regeneration requires that exact existing executable; see codex-protocol/generate.mjs.
export const CODEX_VERSION = protocolSource.version;
const MAX_FRAME_BYTES = 4 * 1024 * 1024;
const MAX_QUESTION_BYTES = 12 * 1024;
const MAX_PENDING = 128;
const MAX_STREAM_BYTES = 12 * 1024;
const execute = promisify(execFile);

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function json(value: unknown): Json {
  return JSON.parse(JSON.stringify(value)) as Json;
}
function requestId(value: unknown): value is RequestId {
  return typeof value === 'string' || (typeof value === 'number' && Number.isSafeInteger(value));
}
// JSON's representation preserves numeric vs string RPC IDs without collisions.
export function questionId(id: RequestId): string { return JSON.stringify(id); }
class RpcRejected extends Error {}
class QuestionResolved extends Error {}

interface PendingRpc {
  resolve(value: unknown): void;
  reject(error: Error): void;
  timer: NodeJS.Timeout;
}

/** Codex app-server is newline-delimited JSON-RPC, not Content-Length framing or exec JSON. */
class CodexRpc {
  private readonly pending = new Map<RequestId, PendingRpc>();
  private readonly decoder = new StringDecoder('utf8');
  private buffer = '';
  private nextId = 0;
  private failure?: Error;
  private ending = false;
  readonly exited: Promise<void>;

  constructor(
    private readonly child: ChildProcessWithoutNullStreams,
    private readonly receive: (packet: Record<string, unknown>) => void,
    private readonly disconnected: (error: Error) => void,
  ) {
    const exit = Promise.withResolvers<void>();
    this.exited = exit.promise;
    child.once('close', () => exit.resolve());
    child.stdout.on('data', (chunk: Buffer) => this.drain(chunk));
    // Drain diagnostics, but never publish unstructured logs, credential dumps, or raw state.
    child.stderr.resume();
    child.stdin.on('error', () => this.fail(new Error('Codex app-server input closed')));
    child.stdout.on('error', () => this.fail(new Error('Codex app-server output failed')));
    child.once('error', () => this.fail(new Error('Unable to launch Codex app-server')));
    child.once('close', () => this.fail(new Error('Codex app-server exited')));
    child.stdout.once('end', () => this.fail(new Error('Codex app-server output closed')));
  }

  private drain(chunk: Buffer): void {
    if (this.failure) return;
    this.buffer += this.decoder.write(chunk);
    let newline: number;
    while ((newline = this.buffer.indexOf('\n')) >= 0) {
      const line = this.buffer.slice(0, newline);
      this.buffer = this.buffer.slice(newline + 1);
      if (Buffer.byteLength(line) > MAX_FRAME_BYTES) {
        this.fail(new Error('Codex protocol frame exceeds its bound'));
        return;
      }
      if (!line.trim()) continue;
      try {
        const packet: unknown = JSON.parse(line);
        if (!object(packet)) throw new Error('Invalid Codex protocol envelope');
        // App-server omits jsonrpc; accept 2.0 when present, never another version.
        if (packet.jsonrpc !== undefined && packet.jsonrpc !== '2.0') throw new Error('Invalid JSON-RPC version');
        if (typeof packet.method === 'string') this.receive(packet);
        else if (requestId(packet.id)) {
          const pending = this.pending.get(packet.id);
          if (!pending) continue;
          this.pending.delete(packet.id);
          clearTimeout(pending.timer);
          if (object(packet.error) && typeof packet.error.code === 'number') {
            pending.reject(new RpcRejected(`Codex rejected request (code ${packet.error.code})`));
          } else if ('result' in packet) pending.resolve(packet.result);
          else pending.reject(new Error('Invalid Codex response envelope'));
        } else throw new Error('Invalid Codex protocol envelope');
      } catch {
        this.fail(new Error('Invalid Codex app-server protocol frame'));
        return;
      }
    }
    if (Buffer.byteLength(this.buffer) > MAX_FRAME_BYTES) this.fail(new Error('Codex protocol frame exceeds its bound'));
  }

  send(packet: unknown): void {
    if (this.failure) throw this.failure;
    if (this.ending) throw new Error('Codex app-server is closing');
    const line = JSON.stringify(packet) + '\n';
    if (Buffer.byteLength(line) > MAX_FRAME_BYTES || this.child.stdin.writableLength > MAX_FRAME_BYTES) {
      throw new Error('Codex protocol output exceeds its bound');
    }
    this.child.stdin.write(line);
  }

  request<T>(method: string, params: unknown, validate: ValidateFunction<T>): Promise<T> {
    if (this.failure || this.ending) return Promise.reject(this.failure ?? new Error('Codex app-server is closing'));
    if (this.pending.size >= MAX_PENDING) return Promise.reject(new Error('Too many pending Codex requests'));
    const id = `phux:${++this.nextId}`;
    const { promise, resolve, reject } = Promise.withResolvers<unknown>();
    const timer = setTimeout(() => {
      this.pending.delete(id);
      reject(new Error(`Codex ${method} response is unknown after timeout`));
    }, 30_000);
    this.pending.set(id, { resolve, reject, timer });
    try { this.send({ id, method, params }); }
    catch (error) {
      clearTimeout(timer);
      this.pending.delete(id);
      reject(error);
    }
    return promise.then((result) => {
      if (!validate(result)) throw new Error(`Codex ${method} response is incompatible with ${CODEX_VERSION}`);
      return result;
    });
  }

  private fail(error: Error): void {
    if (this.failure) return;
    this.failure = error;
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
    this.disconnected(error);
    if (!this.ending && this.child.exitCode === null) {
      this.child.kill('SIGTERM');
      const kill = setTimeout(() => this.child.kill('SIGKILL'), 5000);
      void this.exited.then(() => clearTimeout(kill));
    }
  }

  async close(): Promise<void> {
    if (this.ending) return this.exited;
    this.ending = true;
    this.fail(new Error('Codex app-server closed'));
    // EOF asks the stdio server to shut down. A guard bounds shutdown and reaps this exact child.
    this.child.stdin.end();
    const terminate = setTimeout(() => this.child.kill('SIGTERM'), 1000);
    const kill = setTimeout(() => this.child.kill('SIGKILL'), 5000);
    try { await this.exited; }
    finally { clearTimeout(terminate); clearTimeout(kill); }
  }
}

interface Validators {
  request: ValidateFunction<ServerRequest>;
  notification: ValidateFunction<ServerNotification>;
  initialize: ValidateFunction<InitializeResponse>;
  thread: ValidateFunction<ThreadStartResponse>;
  resume: ValidateFunction<ThreadResumeResponse>;
  turn: ValidateFunction<TurnStartResponse>;
  interrupt: ValidateFunction<TurnInterruptResponse>;
  permissions: ValidateFunction<PermissionsRequestApprovalResponse>;
  elicitation: ValidateFunction<McpServerElicitationRequestResponse>;
}
interface PendingQuestion { controller: AbortController; turnId?: string | null; closeWithTurn: boolean }
type TerminalTurnStatus = Exclude<Turn['status'], 'inProgress'>;

class CodexAdapter implements NativeAdapter {
  readonly capabilities = { prompt: true, interrupt: true, questions: true, follow_up: true };
  providerSessionId?: string;
  private currentTurn?: string;
  private readonly endedTurns = new Map<string, TerminalTurnStatus>();
  private readonly promptOperations = new Map<string, string>();
  private pendingStarts = 0;
  private evictedOutcomes = 0;
  private readonly questions = new Map<RequestId, PendingQuestion>();
  private readonly partials = new Map<string, { role: TranscriptEntry['role']; text: string; truncated: boolean }>();
  private readonly tools = new Map<string, TranscriptEntry>();
  private readonly mode: 'off' | 'default' | 'full';
  private readonly rpc: CodexRpc;
  private closed = false;

  constructor(private readonly context: AdapterContext, child: ChildProcessWithoutNullStreams,
    private readonly ajv: Ajv, private readonly validators: Validators) {
    this.mode = transcriptMode(context.env.PHUX_AGENT_TRANSCRIPT);
    this.rpc = new CodexRpc(child, (packet) => this.receive(packet), (error) => {
      const expected = this.closed;
      this.closed = true;
      for (const pending of this.questions.values()) pending.controller.abort();
      this.questions.clear();
      this.partials.clear();
      this.tools.clear();
      context.closeQuestions('Codex app-server disconnected');
      context.turn(undefined);
      context.connection(false);
      // Disconnect is not a fabricated successful turn completion.
      if (!expected) context.emit({ type: 'notice', level: 'error', text: error.message });
    });
  }

  async initialize(): Promise<void> {
    const params: InitializeParams = {
      clientInfo: { name: 'phux', title: 'phux Native Host', version: '1' },
      capabilities: { experimentalApi: false, requestAttestation: false, explicitGatewayOauth: true },
    };
    await this.rpc.request('initialize', params, this.validators.initialize);
    this.rpc.send({ method: 'initialized' });
    // Omission inherits the installed user's approval and sandbox defaults, not permissive overrides.
    let thread: Thread;
    if (this.context.resumeSessionId) {
      const resume: ThreadResumeParams = { threadId: this.context.resumeSessionId };
      const response = await this.rpc.request('thread/resume', resume, this.validators.resume);
      if (response.thread.id !== this.context.resumeSessionId) throw new Error('Codex resumed an unexpected thread');
      thread = response.thread;
    } else {
      const start: ThreadStartParams = { cwd: this.context.cwd };
      thread = (await this.rpc.request('thread/start', start, this.validators.thread)).thread;
    }
    this.providerSessionId = thread.id;
    this.context.identity(thread.id);
    this.context.connection(true);
    for (const turn of thread.turns) {
      if (turn.status === 'inProgress') this.started(turn.id);
    }
  }

  async prompt(text: string, operationId: string) {
    if (this.closed || !this.providerSessionId) return { disposition: 'refused' as const, detail: 'Codex is disconnected' };
    if (this.pendingStarts + this.promptOperations.size >= MAX_PENDING) {
      return { disposition: 'refused' as const, detail: 'Codex has 128 prompts awaiting correlated turn outcomes' };
    }
    const params: TurnStartParams = { threadId: this.providerSessionId, input: [{ type: 'text', text, text_elements: [] }] };
    const evictedBeforeStart = this.evictedOutcomes;
    this.pendingStarts++;
    try {
      const response = await this.rpc.request('turn/start', params, this.validators.turn);
      const turn = response.turn;
      if (this.promptOperations.has(turn.id)) {
        return { disposition: 'unknown' as const, detail: 'Codex returned a turn already associated with another prompt; input was not retried' };
      }
      this.promptOperations.set(turn.id, operationId);
      const outcome = this.endedTurns.get(turn.id) ?? (turn.status === 'inProgress' ? undefined : turn.status);
      if (outcome !== undefined) this.completePrompt(turn.id, outcome);
      else {
        // A bounded early-outcome cache must not silently turn lost evidence into
        // admission or reopen a turn whose completion may have been evicted.
        if (this.evictedOutcomes !== evictedBeforeStart) {
          return { disposition: 'unknown' as const, detail: 'Codex terminal correlation history exceeded capacity while turn/start was pending; input was not retried' };
        }
        this.started(turn.id);
      }
      return { disposition: 'admitted' as const, detail: `Codex accepted turn ${turn.id}` };
    } catch (error) {
      return { disposition: error instanceof RpcRejected ? 'refused' as const : 'unknown' as const,
        detail: error instanceof Error ? error.message : 'Codex prompt admission is unknown' };
    } finally {
      this.pendingStarts--;
    }
  }

  async interrupt(turnId: string) {
    if (this.closed || !this.providerSessionId || turnId !== this.currentTurn) {
      return { disposition: 'refused' as const, detail: 'That Codex turn is not active' };
    }
    const params: TurnInterruptParams = { threadId: this.providerSessionId, turnId };
    try {
      await this.rpc.request('turn/interrupt', params, this.validators.interrupt);
      return { disposition: 'completed' as const, detail: `Codex acknowledged interrupt for turn ${turnId}; turn termination is reported separately by turn/completed` };
    } catch (error) {
      return { disposition: error instanceof RpcRejected ? 'refused' as const : 'unknown' as const,
        detail: error instanceof Error ? error.message : 'Codex interruption is unknown' };
    }
  }

  private completePrompt(turnId: string, status: TerminalTurnStatus): void {
    if (!this.endedTurns.has(turnId)) this.endedTurns.set(turnId, status);
    if (this.endedTurns.size > MAX_PENDING) {
      const oldest = this.endedTurns.keys().next().value;
      if (oldest !== undefined) {
        this.endedTurns.delete(oldest);
        this.evictedOutcomes++;
      }
    }
    const operationId = this.promptOperations.get(turnId);
    if (operationId === undefined) return;
    this.promptOperations.delete(turnId);
    this.context.result({ operation_id: operationId, disposition: 'completed',
      detail: `Codex turn ${turnId} ended with status ${this.endedTurns.get(turnId) ?? status}` });
  }

  private started(id: string): void {
    if (this.endedTurns.has(id)) return;
    this.currentTurn = id;
    this.context.turn(id);
    this.context.emit({ type: 'state', state: 'working', turn_id: id });
  }

  private receive(packet: Record<string, unknown>): void {
    if ('id' in packet) {
      if (!requestId(packet.id)) return;
      if (!this.validators.request(packet)) {
        this.rpc.send({ id: packet.id, error: { code: -32601, message: 'Unsupported Codex server request or incompatible parameters' } });
        return;
      }
      // Never await a UI callback in the stdout drain. Interrupts and answers remain dispatchable.
      void this.answer(packet).catch(() => {
        this.context.emit({ type: 'notice', level: 'error', text: 'Codex server request response failed' });
      });
      return;
    }
    if (!this.validators.notification(packet)) return;
    this.notification(packet);
  }

  private notification(event: ServerNotification): void {
    const p = event.params;
    if ('threadId' in p && p.threadId !== null && this.providerSessionId && p.threadId !== this.providerSessionId) return;
    switch (event.method) {
      case 'thread/started': break; // The correlated start/resume response owns identity.
      case 'turn/started': this.started(event.params.turn.id); break;
      case 'turn/completed': {
        const turn = event.params.turn;
        if (turn.status === 'inProgress') break;
        this.completePrompt(turn.id, turn.status);
        for (const item of turn.items) this.item(item, true);
        for (const [id, pending] of this.questions) {
          if (pending.closeWithTurn && pending.turnId === turn.id) {
            pending.controller.abort();
            this.questions.delete(id);
            this.context.emit({ type: 'question_closed', id: questionId(id), reason: `Codex turn ${turn.id} ended` });
          }
        }
        if (turn.error) this.context.emit({ type: 'notice', level: 'error', text: keepHead(turn.error.message, 2048) });
        if (this.currentTurn === turn.id) {
          this.currentTurn = undefined;
          this.context.turn(undefined);
          this.context.emit({ type: 'state', state: 'done', turn_id: turn.id });
          this.partials.clear();
          this.tools.clear();
        }
        break;
      }
      case 'thread/status/changed': {
        const status = event.params.status;
        if (status.type === 'active') this.context.emit({ type: 'state', state: status.activeFlags.length ? 'blocked' : 'working', turn_id: this.currentTurn });
        else if (status.type === 'systemError') this.context.emit({ type: 'notice', level: 'error', text: 'Codex thread reported a system error' });
        // An idle thread is not enough evidence to complete a particular turn.
        break;
      }
      case 'serverRequest/resolved': {
        const pending = this.questions.get(event.params.requestId);
        if (pending) {
          pending.controller.abort();
          this.questions.delete(event.params.requestId);
          this.context.emit({ type: 'question_closed', id: questionId(event.params.requestId), reason: 'Codex resolved the server request' });
        }
        break;
      }
      case 'item/started': this.item(event.params.item, false); break;
      case 'item/completed': this.item(event.params.item, true); break;
      case 'item/agentMessage/delta': this.delta(event.params.itemId, 'assistant', event.params.delta); break;
      case 'item/plan/delta': this.delta(event.params.itemId, 'assistant', event.params.delta); break;
      case 'item/reasoning/summaryTextDelta': this.delta(event.params.itemId, 'thinking', event.params.delta); break;
      case 'item/reasoning/textDelta': this.delta(event.params.itemId, 'thinking', event.params.delta); break;
      case 'item/commandExecution/outputDelta':
      case 'item/fileChange/outputDelta': this.toolDelta(event.params.itemId, event.params.delta); break;
      case 'item/mcpToolCall/progress': this.toolDelta(event.params.itemId, event.params.message + '\n'); break;
      case 'warning':
      case 'guardianWarning': this.context.emit({ type: 'notice', level: 'warning', text: keepHead(event.params.message, 2048) }); break;
      case 'error': this.context.emit({ type: 'notice', level: 'error', text: keepHead(event.params.error.message, 2048) }); break;
      case 'thread/closed':
      case 'thread/deleted':
      case 'thread/archived':
        this.closed = true;
        for (const pending of this.questions.values()) pending.controller.abort();
        this.questions.clear();
        this.context.closeQuestions('Codex thread is unavailable');
        this.context.turn(undefined);
        this.context.connection(false);
        this.context.emit({ type: 'notice', level: 'warning', text: 'Codex thread is unavailable' });
        break;
    }
  }

  private publish(entry: TranscriptEntry): TranscriptEntry {
    const bounded = transcriptData('codex', entry).entry;
    if (this.mode !== 'off') this.context.emit({ type: 'transcript', entry: bounded });
    return bounded;
  }
  private delta(id: string, role: TranscriptEntry['role'], delta: string): void {
    if (this.mode === 'off') return;
    if (!this.partials.has(id) && this.partials.size >= MAX_PENDING) return;
    const previous = this.partials.get(id);
    const all = (previous?.text ?? '') + delta;
    const text = keepHead(all, MAX_STREAM_BYTES);
    const truncated = Boolean(previous?.truncated) || text.length < all.length;
    this.partials.set(id, { role, text, truncated });
    this.publish({ id, role, text, truncated, final: false });
  }

  private toolDelta(id: string, delta: string): void {
    if (this.mode !== 'full') return;
    const previous = this.tools.get(id);
    if (!previous?.tool) return;
    const all = previous.tool.output + delta;
    const output = keepTail(all, MAX_TOOL_OUTPUT_BYTES);
    const entry = this.publish({ ...previous, truncated: previous.truncated || output.length < all.length,
      tool: { ...previous.tool, output } });
    this.tools.set(id, entry);
  }

  private item(item: ThreadItem, final: boolean): void {
    if (this.mode === 'off') return;
    const entry = (role: TranscriptEntry['role'], text: string) => this.publish({ id: item.id, role, text, truncated: false, final });
    if (final) this.partials.delete(item.id);
    switch (item.type) {
      case 'userMessage': entry('user', item.content.filter((c) => c.type === 'text').map((c) => c.text).join('\n')); return;
      case 'agentMessage': entry('assistant', item.text); return;
      case 'plan': entry('assistant', item.text); return;
      case 'reasoning': entry('thinking', (item.summary.length ? item.summary : item.content).join('\n')); return;
      case 'enteredReviewMode':
      case 'exitedReviewMode': entry('system', item.review); return;
      case 'contextCompaction': entry('system', 'Codex compacted the thread context'); return;
    }
    let name: string = item.type;
    let summary = '';
    let output = this.tools.get(item.id)?.tool?.output ?? '';
    let failed = false;
    const running = 'status' in item && item.status === 'inProgress';
    switch (item.type) {
      case 'commandExecution': name = 'command'; summary = item.command; output = item.aggregatedOutput ?? output; failed = item.status === 'failed' || item.status === 'declined'; break;
      case 'fileChange': name = 'file_change'; summary = item.changes.map((change) => change.path).join(', '); failed = item.status === 'failed' || item.status === 'declined'; break;
      case 'mcpToolCall':
        name = `${item.server}/${item.tool}`; summary = summarizeArgs(item.arguments); failed = item.status === 'failed' || item.error !== null;
        if (this.mode === 'full') output = item.result?.content.flatMap((c) => object(c) && c.type === 'text' && typeof c.text === 'string' ? [c.text] : []).join('\n') ?? item.error?.message ?? output;
        break;
      case 'dynamicToolCall':
        name = item.tool; summary = summarizeArgs(item.arguments); failed = item.success === false || item.status === 'failed';
        if (this.mode === 'full') output = item.contentItems?.flatMap((c) => c.type === 'inputText' ? [c.text] : []).join('\n') ?? output;
        break;
      case 'collabAgentToolCall': name = item.tool; summary = item.receiverThreadIds.join(', '); failed = item.status === 'failed'; break;
      case 'webSearch': summary = item.query ?? ''; break;
      case 'imageView': summary = item.path; break;
      case 'subAgentActivity': summary = item.agentPath; break;
      case 'functionCallOutput':
        name = item.namespace ? `${item.namespace}/${item.name}` : item.name;
        if (this.mode === 'full') output = typeof item.output === 'string' ? item.output : item.output.flatMap((c) => c.type === 'input_text' ? [c.text] : []).join('\n');
        break;
      case 'imageGeneration': summary = item.savedPath ?? 'Image generation'; failed = item.failure !== null; break;
      case 'sleep': name = 'clock.sleep'; summary = `${item.durationMs} ms`; break;
      default: return;
    }
    const projected = this.publish({ id: item.id, role: 'tool', text: '', truncated: false, final,
      tool: { name, call_id: item.id, summary, status: failed ? 'error' : !final || running ? 'running' : 'ok', output: this.mode === 'full' ? output : '' } });
    if (final) this.tools.delete(item.id);
    else if (this.tools.size < MAX_PENDING) this.tools.set(item.id, projected);
  }

  private async ask(id: RequestId, question: NativeQuestion, turnId?: string | null, closeWithTurn = true): Promise<Json> {
    if (this.questions.size >= MAX_PENDING || Buffer.byteLength(JSON.stringify(question)) > MAX_QUESTION_BYTES) {
      throw new Error('Codex question exceeds native UI bounds');
    }
    if (this.questions.has(id)) throw new Error('Duplicate active Codex request ID');
    const pending = { controller: new AbortController(), turnId, closeWithTurn };
    this.questions.set(id, pending);
    try {
      const response = await this.context.question(question, pending.controller.signal);
      if (pending.controller.signal.aborted || this.closed) throw new QuestionResolved();
      return response;
    } catch (error) {
      if (pending.controller.signal.aborted) throw new QuestionResolved();
      throw error;
    }
  }

  private async choose<T>(request: ServerRequest, decisions: T[], cancelled: T, title: string, message: string): Promise<T> {
    const response = await this.ask(request.id, {
      id: questionId(request.id), kind: 'permission', title, message,
      options: decisions.map((decision) => ({ id: typeof decision === 'string' ? decision : JSON.stringify(decision), label: typeof decision === 'string' ? decision : JSON.stringify(decision) })),
      questions: [], fields: json({ rpc_request_id: request.id, ...request.params }),
    }, 'turnId' in request.params ? request.params.turnId : undefined);
    if (object(response) && response.cancelled === true) return cancelled;
    if (!object(response) || typeof response.option_id !== 'string') throw new Error('Codex permission requires an explicit option ID');
    const selected = decisions.find((decision) => (typeof decision === 'string' ? decision : JSON.stringify(decision)) === response.option_id);
    if (selected === undefined) throw new Error('Unknown Codex permission option');
    return selected;
  }

  private async answer(request: ServerRequest): Promise<void> {
    try {
      if ('threadId' in request.params && this.providerSessionId && request.params.threadId !== this.providerSessionId) throw new Error('Codex request targets another thread');
      let result: unknown;
      switch (request.method) {
        case 'item/commandExecution/requestApproval': {
          const p = request.params;
          const decisions: CommandExecutionApprovalDecision[] = ['accept', 'acceptForSession', 'decline', 'cancel'];
          if (p.proposedExecpolicyAmendment) decisions.push({ acceptWithExecpolicyAmendment: { execpolicy_amendment: p.proposedExecpolicyAmendment } });
          for (const amendment of p.proposedNetworkPolicyAmendments ?? []) decisions.push({ applyNetworkPolicyAmendment: { network_policy_amendment: amendment } });
          result = { decision: await this.choose(request, decisions, 'cancel', 'Codex command permission', p.command ?? p.reason ?? p.kind) };
          break;
        }
        case 'item/fileChange/requestApproval': {
          const decisions: FileChangeApprovalDecision[] = ['accept', 'acceptForSession', 'decline', 'cancel'];
          result = { decision: await this.choose(request, decisions, 'cancel', 'Codex file permission', request.params.reason ?? request.params.itemId) };
          break;
        }
        case 'item/permissions/requestApproval': {
          const p = request.params;
          const granted = { ...(p.permissions.network ? { network: p.permissions.network } : {}), ...(p.permissions.fileSystem ? { fileSystem: p.permissions.fileSystem } : {}) };
          const decisions: PermissionsRequestApprovalResponse[] = [
            { permissions: granted, scope: 'turn' }, { permissions: granted, scope: 'session' }, { permissions: {}, scope: 'turn' },
          ];
          const selected = await this.choose(request, decisions, { permissions: {}, scope: 'turn' }, 'Codex permissions', p.reason ?? 'Review the requested permissions and scope');
          if (!this.validators.permissions(selected)) throw new Error('Invalid Codex permissions response');
          result = selected;
          break;
        }
        case 'item/tool/requestUserInput': {
          const p = request.params;
          // Secret fields require a protected UI, which this protocol does not advertise.
          if (p.questions.some((q) => q.isSecret)) throw new Error('Secret Codex user input is unsupported');
          const response = await this.ask(request.id, {
            id: questionId(request.id), kind: 'ask', title: 'Codex user input', message: '', options: [],
            questions: p.questions.map((q) => ({ id: q.id, header: q.header, question: q.question, multi: false,
              options: (q.options ?? []).map((option) => ({ id: option.label, label: option.label, description: option.description })) })),
            fields: json({ rpc_request_id: request.id, ...p }),
          }, p.turnId, p.isBlocking);
          // The official app-server maps a cancelled input request to an empty answers map.
          if (object(response) && response.cancelled === true) {
            result = { answers: {} } satisfies ToolRequestUserInputResponse;
            break;
          }
          if (!object(response) || !Array.isArray(response.answers)) throw new Error('Invalid Codex user input response');
          const answers: ToolRequestUserInputResponse['answers'] = {};
          for (const answer of response.answers) {
            if (!object(answer) || typeof answer.id !== 'string' || !Array.isArray(answer.selected_options)) throw new Error('Invalid Codex answer');
            const q = p.questions.find((candidate) => candidate.id === answer.id);
            if (!q || Object.hasOwn(answers, q.id) || answer.selected_options.length > 1) throw new Error('Invalid Codex question ID or multiple selections');
            const chosen: string[] = [];
            for (const option of answer.selected_options) {
              if (typeof option !== 'string' || !q.options?.some((candidate) => candidate.label === option)) throw new Error('Unknown Codex input option');
              chosen.push(option);
            }
            if (answer.custom_input !== undefined) {
              if (typeof answer.custom_input !== 'string' || (!q.isOther && q.options !== null)) throw new Error('Custom Codex input is unsupported for this question');
              if (chosen.length) throw new Error('Choose an option or custom input, not both');
              chosen.push(answer.custom_input);
            }
            if (!chosen.length) throw new Error('Codex input question is unanswered');
            Object.defineProperty(answers, q.id, { value: { answers: chosen }, enumerable: true });
          }
          if (Object.keys(answers).length !== p.questions.length) throw new Error('Codex input response is incomplete');
          result = { answers } satisfies ToolRequestUserInputResponse;
          break;
        }
        case 'mcpServer/elicitation/request': {
          const p = request.params;
          if (p.mode !== 'form') throw new Error(`Codex MCP ${p.mode} UI is unsupported`);
          const schema = json(p.requestedSchema);
          if (!object(schema) || schema.type !== 'object' || !object(schema.properties)) throw new Error('Unsupported Codex form schema');
          for (const field of Object.values(schema.properties)) {
            if (!object(field) || !['string', 'boolean', 'number', 'integer', 'array'].includes(String(field.type))) throw new Error('Unsupported Codex form field');
            if (field.type === 'array' && (!object(field.items) || field.items.type !== 'string')) throw new Error('Unsupported Codex form array');
          }
          const validate = this.ajv.compile(p.requestedSchema);
          const response = await this.ask(request.id, { id: questionId(request.id), kind: 'form', title: p.serverName,
            message: p.message, options: [], questions: [], fields: schema }, p.turnId, false);
          if (object(response) && response.cancelled === true) {
            result = { action: 'cancel', content: null, _meta: null } satisfies McpServerElicitationRequestResponse;
            break;
          }
          if (!object(response) || !object(response.values) || !validate(response.values)) throw new Error('Codex form response does not satisfy the provider schema');
          const accepted: McpServerElicitationRequestResponse = { action: 'accept', content: json(response.values), _meta: null };
          if (!this.validators.elicitation(accepted)) throw new Error('Invalid Codex elicitation response');
          result = accepted;
          break;
        }
        default: throw new Error(`Unsupported Codex server request: ${request.method}`);
      }
      if (!this.closed) this.rpc.send({ id: request.id, result });
    } catch (error) {
      // The server may already have resolved a cancelled request. Do not send a late reply.
      if (this.closed || error instanceof QuestionResolved) return;
      this.rpc.send({ id: request.id, error: { code: -32602, message: error instanceof Error ? error.message : 'Codex native UI request refused' } });
      this.context.emit({ type: 'notice', level: 'warning', text: 'Codex native UI request was refused or cancelled' });
    }
  }

  async close(): Promise<void> {
    this.closed = true;
    for (const pending of this.questions.values()) pending.controller.abort();
    this.questions.clear();
    this.context.closeQuestions('Codex app-server closed');
    this.context.connection(false);
    await this.rpc.close();
  }
}

export async function createAdapter(context: AdapterContext): Promise<NativeAdapter> {
  const [executable, ...args] = context.command;
  if (!executable || !isAbsolute(executable)) throw new Error('Codex requires an explicit absolute executable path');
  const version = await execute(executable, [...args, '--version'], { cwd: context.cwd, env: context.env, timeout: 10_000, maxBuffer: 4096 });
  if (version.stdout.trim() !== `codex-cli ${CODEX_VERSION}`) {
    throw new Error(`Codex app-server requires executable version ${CODEX_VERSION}; regenerate and review the official protocol before changing the pin`);
  }
  // SDK import must follow isolated-runtime resolution hook and sanitized process environment.
  const { Ajv: AjvConstructor } = await import('ajv');
  const { default: addFormats } = await import('ajv-formats');
  const ajv = new AjvConstructor({ strict: false, allErrors: false, validateFormats: true });
  addFormats(ajv);
  for (const name of ['uint', 'uint64']) ajv.addFormat(name, { type: 'number', validate: (n: number) => Number.isSafeInteger(n) && n >= 0 });
  ajv.addFormat('uint16', { type: 'number', validate: (n: number) => Number.isInteger(n) && n >= 0 && n <= 65535 });
  ajv.addFormat('uint32', { type: 'number', validate: (n: number) => Number.isInteger(n) && n >= 0 && n <= 4294967295 });
  ajv.addFormat('int32', { type: 'number', validate: (n: number) => Number.isInteger(n) && n >= -2147483648 && n <= 2147483647 });
  ajv.addFormat('int64', { type: 'number', validate: (n: number) => Number.isSafeInteger(n) });
  ajv.addFormat('double', { type: 'number', validate: Number.isFinite });
  const validators: Validators = {
    request: ajv.compile<ServerRequest>(requestSchema), notification: ajv.compile<ServerNotification>(notificationSchema),
    initialize: ajv.compile<InitializeResponse>(initializeSchema), thread: ajv.compile<ThreadStartResponse>(threadSchema),
    resume: ajv.compile<ThreadResumeResponse>(resumeSchema),
    turn: ajv.compile<TurnStartResponse>(turnSchema), interrupt: ajv.compile<TurnInterruptResponse>(interruptSchema),
    permissions: ajv.compile<PermissionsRequestApprovalResponse>(permissionsSchema), elicitation: ajv.compile<McpServerElicitationRequestResponse>(elicitationSchema),
  };
  const child = spawn(executable, [...args, 'app-server', '--listen', 'stdio://'], { cwd: context.cwd, env: context.env, stdio: ['pipe', 'pipe', 'pipe'], detached: false });
  const adapter = new CodexAdapter(context, child, ajv, validators);
  try { await adapter.initialize(); return adapter; }
  catch (error) { await adapter.close(); throw error; }
}
