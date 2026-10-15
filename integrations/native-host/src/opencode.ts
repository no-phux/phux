import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import type { OpenCodeClient, FormDetail, FormField, FormAnswer, PermissionRequest, SessionMessageInfo } from '@opencode/client';
import type { AdapterContext, Json, NativeAdapter, NativeCapabilities, NativeCommandResult, NativeQuestion, TranscriptEntry } from './types.js';
import { summarizeArgs, transcriptData } from '../../runtime/src/transcript.js';

const VERSION = '2.0.26';
const capabilities: NativeCapabilities = { prompt: true, interrupt: true, questions: true, follow_up: true };
type Receipt = { disposition: NativeCommandResult['disposition']; detail?: string };
type Pending = { question: NativeQuestion; controller: AbortController };

/**
 * Requires the exact 2.0.26 executable, an installed private @opencode/client runtime,
 * and provider credentials/configuration already supplied by the operator. No auth
 * flow or package installation is performed. Service config/state is private;
 * OPENCODE_CONFIG, when supplied, still names the operator's model configuration.
 *
 * Service.ensure in 2.0.26 hardcodes detached:true and may replace incumbents. It
 * cannot satisfy guardian ownership. Spawn the same official service command in
 * our process group, discover ONLY its private registration, and reap our child.
 */
export async function createAdapter(context: AdapterContext): Promise<NativeAdapter> {
  if (!context.command[0]) throw new Error('OpenCode native mode requires an explicit executable');
  // SDK import must follow isolated-runtime resolution hook and sanitized process environment.
  const [{ OpenCode }, { Service }] = await Promise.all([import('@opencode/client'), import('@opencode/client/service')]);
  const directory = await mkdtemp(join(tmpdir(), 'phux-opencode-'));
  const launch = privateServiceLaunch(context, directory);
  const child = spawn(launch.command, launch.args, {
    cwd: context.cwd, env: launch.env, detached: false, stdio: 'ignore', windowsHide: true,
  });
  let exited = false;
  let failed = false;
  const reaped = new Promise<void>((resolve) => {
    child.once('error', () => { failed = true; exited = true; resolve(); });
    child.once('close', () => { exited = true; resolve(); });
  });
  let stopped = false;
  const stop = async () => {
    if (stopped) return;
    stopped = true;
    if (!exited) {
      child.kill('SIGTERM');
      await Promise.race([reaped, delay(5_000, undefined, { ref: false })]);
      if (!exited) child.kill('SIGKILL');
    }
    await reaped;
    await rm(directory, { recursive: true, force: true });
  };
  try {
    const deadline = Date.now() + 30_000;
    while (Date.now() < deadline) {
      if (exited) throw new Error(failed ? 'OpenCode executable could not start' : 'OpenCode private service exited before readiness');
      // Do not trust a registration for another process, even inside our directory.
      const registration: unknown = await readFile(launch.file, 'utf8').then((text): unknown => JSON.parse(text)).catch(() => undefined);
      if (record(registration) && registration.pid === child.pid) {
        const endpoint = await Service.discover({ file: launch.file, version: VERSION });
        if (endpoint) {
          const client = OpenCode.make({ baseUrl: endpoint.url, headers: Service.headers(endpoint) });
          const adapter = new OpenCodeAdapter(context, client, stop);
          await adapter.start();
          void reaped.then(() => adapter.serviceExited());
          return adapter;
        }
      }
      await delay(100);
    }
    throw new Error('OpenCode 2.0.26 private service did not become ready');
  } catch (error) {
    await stop();
    throw error;
  }
}

/** Kept explicit so launch isolation and exact argv are testable without a CLI. */
export function privateServiceLaunch(context: AdapterContext, directory: string): {
  command: string; args: string[]; file: string; env: NodeJS.ProcessEnv;
} {
  const [command, ...prefix] = context.command;
  if (!command) throw new Error('OpenCode native mode requires an explicit executable');
  // v2.0.26 ServiceConfig uses Global.state/service.json (latest channel).
  // Private config also prevents ServiceConfig.password from writing user config.
  const state = join(directory, 'state');
  const env: NodeJS.ProcessEnv = {
    ...context.env,
    XDG_STATE_HOME: state,
    OPENCODE_CONFIG_DIR: join(directory, 'config'),
    PHUX_AGENT_REPORTING: '0',
  };
  delete env.PHUX_NATIVE_BINDING;
  delete env.PHUX_NATIVE_BINDING_PROOF;
  delete env.PHUX_NATIVE_ACTIVATION;
  delete env.PHUX_NATIVE_AGENT_ID;
  delete env.OPENCODE_PTY_HANDOFF;
  return {
    command, args: [...prefix, 'serve', '--service', '--hostname', '127.0.0.1', '--port', '0'],
    file: join(state, 'opencode', 'service.json'), env,
  };
}

/** Owns one actual session. Reconnection only re-subscribes and reads snapshots. */
export class OpenCodeAdapter implements NativeAdapter {
  readonly capabilities = capabilities;
  providerSessionId?: string;
  private readonly lifetime = new AbortController();
  private readonly pending = new Map<string, Pending>();
  private readonly transcript = new Map<string, string>();
  private readonly unsupported = new Set<string>();
  private turnId?: string;
  private sequence?: number;
  private running = false;
  private connected = false;
  private streamReady = false;
  private stream?: AbortController;
  private dirty = false;
  private refreshing?: Promise<void>;
  private events?: Promise<void>;
  private closed = false;

  constructor(private readonly context: AdapterContext, private readonly client: OpenCodeClient, private readonly stopService: () => Promise<void>) {}

  async start(): Promise<void> {
    // No permission overrides: OpenCode retains its normal human permission policy.
    const session = this.context.resumeSessionId
      ? await this.client.session.get({ sessionID: this.context.resumeSessionId }, this.request())
      : await this.client.session.create({ location: { directory: this.context.cwd } }, this.request());
    if (this.context.resumeSessionId && session.id !== this.context.resumeSessionId) throw new Error('OpenCode returned a different session than the requested resume ID');
    this.providerSessionId = session.id;
    this.context.identity(session.id);
    this.events = this.drain();
    try { await this.refresh(); } catch (error) { await this.close(); throw error; }
  }

  private request(): { signal: AbortSignal } {
    return { signal: AbortSignal.any([this.lifetime.signal, AbortSignal.timeout(30_000)]) };
  }

  private sessionID(): string {
    if (!this.providerSessionId) throw new Error('OpenCode session is not initialized');
    return this.providerSessionId;
  }

  private view(): void {
    this.context.connection(this.connected && !this.closed);
    this.context.emit({ type: 'view', view: {
      activation: this.context.activation, backend: 'node',
      capabilities, connected: this.connected && !this.closed,
      provider_session_id: this.providerSessionId, turn_id: this.turnId,
      questions: [...this.pending.values()].map((item) => item.question),
    } });
  }

  private state(): void {
    this.context.turn(this.turnId);
    this.context.emit({ type: 'state', state: this.pending.size ? 'blocked' : this.running ? 'working' : 'done', turn_id: this.turnId });
    this.view();
  }

  private notice(text: string): void {
    this.context.emit({ type: 'notice', level: 'error', text });
  }

  private async drain(): Promise<void> {
    while (!this.closed) {
      this.streamReady = false;
      const stream = new AbortController();
      this.stream = stream;
      try {
        for await (const event of this.client.event.subscribe({ signal: AbortSignal.any([this.lifetime.signal, stream.signal]) })) {
          if (this.closed) break;
          if (event.type === 'server.connected') {
            this.streamReady = true;
            this.scheduleRefresh();
            continue;
          }
          // The volatile feed is a wakeup, never our durable source of truth.
          // No human-response Promise or HTTP snapshot blocks this event drain.
          if ('data' in event && record(event.data) && (
            ('sessionID' in event.data && event.data.sessionID === this.providerSessionId) ||
            (event.type === 'form.created' && record(event.data.form) && event.data.form.sessionID === this.providerSessionId)
          )) this.scheduleRefresh();
        }
      } catch {
        if (this.closed) return;
      }
      if (this.closed) return;
      this.streamReady = false;
      this.connected = false;
      this.view();
      try { await delay(250, undefined, { signal: this.lifetime.signal }); } catch { return; }
      // Reuse the client, service and session. Never ensure/create/prompt here.
    }
  }

  private scheduleRefresh(): void {
    void this.refresh().catch(() => {
      if (!this.closed) {
        this.connected = false;
        this.view();
        this.notice('OpenCode session snapshot unavailable; no input was replayed');
        this.stream?.abort();
      }
    });
  }

  private refresh(): Promise<void> {
    this.dirty = true;
    if (this.refreshing) return this.refreshing;
    this.refreshing = (async () => {
      while (this.dirty && !this.closed) {
        this.dirty = false;
        await this.snapshot();
      }
    })().finally(() => { this.refreshing = undefined; });
    return this.refreshing;
  }

  private async snapshot(): Promise<void> {
    const sessionID = this.sessionID();
    // Durable execution events supply the real execution-start event ID. The
    // active endpoint has no turn ID; do not invent one from operation numbers.
    for await (const event of this.client.session.log({ sessionID, after: this.sequence, follow: false }, this.request())) {
      if (event.type === 'log.synced') break;
      this.sequence = Math.max(this.sequence ?? 0, event.durable.seq);
      if (event.type === 'session.execution.started') this.turnId = event.id;
      if (event.type === 'session.execution.succeeded' || event.type === 'session.execution.failed' || event.type === 'session.execution.interrupted') {
        this.turnId = undefined;
        if (event.type === 'session.execution.failed') this.notice('OpenCode execution failed');
      }
    }
    const [session, active, permissions, forms] = await Promise.all([
      this.client.session.get({ sessionID }, this.request()),
      this.client.session.active(this.request()),
      this.client.permission.list({ sessionID }, this.request()),
      this.client.session.form.list({ sessionID }, this.request()),
    ]);
    if (session.id !== sessionID) throw new Error('OpenCode returned a different session');
    const details = await Promise.all(forms.map((form) => this.client.session.form.get({ sessionID, formID: form.id }, this.request())));
    let cursor: string | undefined;
    do {
      const messages = await this.client.message.list({ sessionID, order: 'asc', limit: 100, cursor }, this.request());
      if (this.closed) return;
      for (const message of messages.data) this.publishMessage(message);
      const next = messages.cursor.next ?? undefined;
      if (next === cursor && next !== undefined) throw new Error('OpenCode returned a repeated message cursor');
      cursor = next;
    } while (cursor !== undefined);
    if (this.closed) return;
    this.connected = this.streamReady;
    this.running = active[sessionID]?.type === 'running';
    if (!this.running) this.turnId = undefined;
    const current = new Set([...permissions.map((item) => item.id), ...details.filter((item) => item.state.status === 'pending').map((item) => item.id)]);
    for (const id of this.pending.keys()) if (!current.has(id)) this.closeQuestion(id, 'Provider no longer has this request pending');
    for (const permission of permissions) this.openPermission(permission);
    for (const form of details) if (form.state.status === 'pending') this.openForm(form);
    this.state();
  }

  private publish(entry: TranscriptEntry): void {
    const bounded = transcriptData(this.context.provider, entry).entry;
    const fingerprint = JSON.stringify(bounded);
    if (this.transcript.get(entry.id) === fingerprint) return;
    this.transcript.set(entry.id, fingerprint);
    this.context.emit({ type: 'transcript', entry: bounded });
  }

  private publishMessage(message: SessionMessageInfo): void {
    if (message.type === 'assistant') {
      const final = message.time.completed !== undefined;
      const text = message.content.filter((item) => item.type === 'text').map((item) => item.text).join('');
      this.publish({ id: message.id, role: 'assistant', text, truncated: false, final });
      const thinking = message.content.filter((item) => item.type === 'reasoning').map((item) => item.text).join('');
      if (thinking) this.publish({ id: `${message.id}:reasoning`, role: 'thinking', text: thinking, truncated: false, final });
      for (const item of message.content) {
        if (item.type !== 'tool') continue;
        const state = item.state;
        const finished = state.status === 'completed' || state.status === 'error';
        const output = finished ? (state.content ?? []).flatMap((part) => part.type === 'text' && 'text' in part ? [part.text] : []).join('\n') : '';
        this.publish({ id: item.id, role: 'tool', text: state.status === 'error' ? state.error.message : '', truncated: false, final: finished,
          tool: { name: item.name, call_id: item.id, summary: summarizeArgs(state.input), status: state.status === 'error' ? 'error' : finished ? 'ok' : 'running', output },
        });
      }
    } else if (message.type === 'user' || message.type === 'system' || message.type === 'synthetic' || message.type === 'skill') {
      this.publish({ id: message.id, role: message.type === 'user' ? 'user' : 'system', text: message.text, truncated: false, final: true });
    } else if (message.type === 'shell') {
      this.publish({ id: message.id, role: 'tool', text: '', truncated: message.output?.truncated ?? false, final: message.status !== 'running',
        tool: { name: 'shell', call_id: message.shellID, summary: message.command, status: message.status === 'running' ? 'running' : message.status === 'exited' && message.exit === 0 ? 'ok' : 'error', output: message.output?.output ?? '' },
      });
    }
  }

  private closeQuestion(id: string, reason: string): void {
    const item = this.pending.get(id);
    if (!item) return;
    this.pending.delete(id);
    item.controller.abort();
    this.context.emit({ type: 'question_closed', id, reason });
  }

  private open(question: NativeQuestion, reply: (answer: Json) => Promise<void>): void {
    if (this.pending.has(question.id)) return;
    if (Buffer.byteLength(JSON.stringify(question), 'utf8') > 12 * 1024) {
      this.notice('OpenCode request exceeds the native question payload bound; it was not approved');
      return;
    }
    const pending: Pending = { question, controller: new AbortController() };
    this.pending.set(question.id, pending);
    void this.context.question(question, pending.controller.signal).then(async (answer) => {
      if (this.closed || this.pending.get(question.id) !== pending) return;
      await reply(answer);
      if (this.closed || this.pending.get(question.id) !== pending) return;
      this.closeQuestion(question.id, 'OpenCode acknowledged the response');
      this.scheduleRefresh();
    }).catch(() => {
      if (this.closed || pending.controller.signal.aborted) return;
      this.pending.delete(question.id);
      this.notice('OpenCode did not acknowledge the answer; refreshing the actual pending request, without resending the answer');
      this.scheduleRefresh();
    });
  }

  private openPermission(permission: PermissionRequest): void {
    if (permission.sessionID !== this.sessionID()) return;
    this.open({ id: permission.id, kind: 'permission', title: permission.action,
      message: [permission.message, ...permission.resources].filter((item) => item !== undefined).join('\n'),
      // Exact API decision IDs and English labels from v2.0.26
      // packages/tui/src/util/permission.ts:permissionOptionLabel. No default.
      options: [{ id: 'once', label: 'Allow once' }, { id: 'always', label: 'Always allow' }, { id: 'reject', label: 'Reject' }], questions: [],
      fields: { action: permission.action, resources: permission.resources, save: permission.save ?? [], source: permission.source ? { type: permission.source.type, messageID: permission.source.messageID, id: permission.source.id } : null },
    }, async (answer) => {
      if (record(answer) && answer.cancelled === true) {
        await this.client.permission.reply({ sessionID: this.sessionID(), requestID: permission.id, decision: 'reject' }, this.request());
        return;
      }
      if (!record(answer) || (answer.option_id !== 'once' && answer.option_id !== 'always' && answer.option_id !== 'reject')) throw new Error('Invalid OpenCode permission decision');
      await this.client.permission.reply({ sessionID: this.sessionID(), requestID: permission.id, decision: answer.option_id }, this.request());
    });
  }

  private openForm(form: FormDetail): void {
    if (form.sessionID !== this.sessionID() || this.pending.has(form.id) || this.unsupported.has(form.id)) return;
    let fields: Json;
    try { fields = formSchema(form.fields); } catch {
      this.unsupported.add(form.id);
      this.notice('OpenCode requested an unsupported external, conditional, hidden or oversized form; cancelling it without approval');
      void this.client.session.form.cancel({ sessionID: this.sessionID(), formID: form.id, message: 'This native client cannot safely display the requested form' }, this.request())
        .then(() => this.scheduleRefresh()).catch(() => { this.unsupported.delete(form.id); this.notice('OpenCode did not acknowledge cancellation of the unsupported form'); });
      return;
    }
    this.open({ id: form.id, kind: 'form', title: form.title, message: '', options: [], questions: [], fields }, async (answer) => {
      if (record(answer) && answer.cancelled === true) {
        await this.client.session.form.cancel({ sessionID: this.sessionID(), formID: form.id, message: 'Cancelled by the user' }, this.request());
        return;
      }
      const values = validateFormAnswer(form.fields, answer);
      await this.client.session.form.reply({ sessionID: this.sessionID(), formID: form.id, answer: values }, this.request());
    });
  }

  async prompt(text: string, operationId: string): Promise<Receipt> {
    if (this.closed || !this.connected) return { disposition: 'refused', detail: 'OpenCode event connection is unavailable' };
    try {
      const receipt = await this.client.session.prompt({ sessionID: this.sessionID(), text }, this.request());
      if (receipt.sessionID !== this.sessionID()) return { disposition: 'unknown', detail: 'OpenCode returned an unexpected session for prompt admission' };
      this.scheduleRefresh();
      return { disposition: 'queued', detail: `OpenCode inbox ${receipt.id}; delivery=${receipt.delivery}; host operation=${operationId}. Not a completed model turn.` };
    } catch (error) {
      return mutationFailure(error, 'OpenCode prompt admission was not acknowledged; it was not retried');
    }
  }

  async interrupt(turnId: string): Promise<Receipt> {
    if (this.closed || !this.connected) return { disposition: 'refused', detail: 'OpenCode event connection is unavailable' };
    try { await this.refresh(); } catch { return { disposition: 'refused', detail: 'Cannot establish the current OpenCode execution' }; }
    if (!this.running || !this.turnId || turnId !== this.turnId) return { disposition: 'refused', detail: 'OpenCode execution ID is no longer current' };
    try {
      const result = await this.client.session.interrupt({ sessionID: this.sessionID() }, this.request());
      this.scheduleRefresh();
      return result.interrupted ? { disposition: 'completed', detail: 'OpenCode acknowledged execution interruption' } : { disposition: 'refused', detail: 'OpenCode reported no execution was interrupted' };
    } catch (error) {
      return mutationFailure(error, 'OpenCode interruption was not acknowledged; it was not retried');
    }
  }

  serviceExited(): void {
    if (this.closed) return;
    this.closed = true;
    this.connected = false;
    this.lifetime.abort();
    for (const id of this.pending.keys()) this.closeQuestion(id, 'OpenCode private service exited');
    this.context.closeQuestions('OpenCode private service exited');
    this.notice('OpenCode private service exited; native control is unavailable');
    this.view();
  }

  async close(): Promise<void> {
    this.closed = true;
    this.connected = false;
    this.lifetime.abort();
    for (const id of this.pending.keys()) this.closeQuestion(id, 'OpenCode native host closed');
    this.context.closeQuestions('OpenCode native host closed');
    this.view();
    await this.stopService();
    await this.events;
    await this.refreshing?.catch(() => {});
  }
}

function mutationFailure(error: unknown, detail: string): Receipt {
  // Declared server errors are explicit refusals. Transport/protocol errors can
  // follow a successful mutation and must remain Unknown, never be auto-retried.
  return { disposition: record(error) && typeof error._tag === 'string' ? 'refused' : 'unknown', detail };
}

function record(value: unknown): value is Record<string, Json | undefined> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** Translate the actual supported provider field vocabulary, not a made-up form. */
export function formSchema(fields: FormField[]): Json {
  const properties: Record<string, Json> = {};
  const required: string[] = [];
  for (const field of fields) {
    if (field.type === 'external' || field.hidden || field.when?.length || Object.hasOwn(properties, field.key)) throw new Error('Unsupported OpenCode form presentation');
    const schema: Record<string, Json> = { type: field.type === 'multiselect' ? 'array' : field.type, title: field.title ?? field.key };
    if (field.description !== undefined) schema.description = field.description;
    if (field.default !== undefined) schema.default = field.default;
    if (field.required) required.push(field.key);
    if (field.type === 'string') {
      if (field.minLength !== undefined) schema.minLength = field.minLength;
      if (field.maxLength !== undefined) schema.maxLength = field.maxLength;
      if (field.pattern !== undefined) { new RegExp(field.pattern); schema.pattern = field.pattern; }
      if (field.format !== undefined) schema.format = field.format;
      if (field.options && !field.custom) schema.oneOf = field.options.map((option) => ({ const: option.value, title: option.label, ...(option.description ? { description: option.description } : {}) }));
    } else if (field.type === 'number' || field.type === 'integer') {
      if (field.default !== undefined && (typeof field.default !== 'number' || !Number.isFinite(field.default))) throw new Error('Nonfinite form default');
      if (field.minimum !== undefined) { if (typeof field.minimum !== 'number' || !Number.isFinite(field.minimum)) throw new Error('Nonfinite form bound'); schema.minimum = field.minimum; }
      if (field.maximum !== undefined) { if (typeof field.maximum !== 'number' || !Number.isFinite(field.maximum)) throw new Error('Nonfinite form bound'); schema.maximum = field.maximum; }
    } else if (field.type === 'multiselect') {
      schema.items = field.custom ? { type: 'string' } : { type: 'string', oneOf: field.options.map((option) => ({ const: option.value, title: option.label, ...(option.description ? { description: option.description } : {}) })) };
      if (field.minItems !== undefined) schema.minItems = field.minItems;
      if (field.maxItems !== undefined) schema.maxItems = field.maxItems;
    }
    Object.defineProperty(properties, field.key, { value: schema, enumerable: true });
  }
  const schema: Json = { type: 'object', properties, required, additionalProperties: false };
  if (Buffer.byteLength(JSON.stringify(schema), 'utf8') > 10 * 1024) throw new Error('OpenCode form exceeds native payload bound');
  return schema;
}

export function validateFormAnswer(fields: FormField[], response: Json): FormAnswer {
  if (!record(response) || !record(response.values)) throw new Error('Expected OpenCode form values');
  const values = response.values;
  const answer: FormAnswer = {};
  if (Object.keys(values).some((key) => !fields.some((field) => field.key === key))) throw new Error('Unknown OpenCode form field');
  for (const field of fields) {
    if (field.type === 'external') throw new Error('External form fields are unsupported');
    const value = Object.hasOwn(values, field.key) ? values[field.key] : undefined;
    if (value === undefined) { if (field.required) throw new Error('Required OpenCode form field missing'); continue; }
    if (field.required && (value === '' || (Array.isArray(value) && value.length === 0))) throw new Error('Required OpenCode form field empty');
    if (field.type === 'string') {
      if (typeof value !== 'string' || (field.minLength !== undefined && value.length < field.minLength) || (field.maxLength !== undefined && value.length > field.maxLength) || (field.pattern !== undefined && !new RegExp(field.pattern).test(value))) throw new Error('Invalid OpenCode text field');
      if (field.options && !field.custom && !field.options.some((option) => option.value === value)) throw new Error('Unknown OpenCode form option');
      if (field.format === 'email' && !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(value)) throw new Error('Invalid email');
      if (field.format === 'uri') new URL(value);
      if (field.format === 'date' && (!/^\d{4}-\d{2}-\d{2}$/.test(value) || Number.isNaN(Date.parse(`${value}T00:00:00.000Z`)) || new Date(`${value}T00:00:00.000Z`).toISOString().slice(0, 10) !== value)) throw new Error('Invalid date');
      if (field.format === 'date-time' && Number.isNaN(Date.parse(value))) throw new Error('Invalid date-time');
    } else if (field.type === 'number' || field.type === 'integer') {
      if (typeof value !== 'number' || !Number.isFinite(value) || (field.type === 'integer' && !Number.isInteger(value)) || (typeof field.minimum === 'number' && value < field.minimum) || (typeof field.maximum === 'number' && value > field.maximum)) throw new Error('Invalid OpenCode number field');
    } else if (field.type === 'boolean') {
      if (typeof value !== 'boolean') throw new Error('Invalid OpenCode boolean field');
    } else {
      if (!Array.isArray(value) || !value.every((item): item is string => typeof item === 'string') || (field.minItems !== undefined && value.length < field.minItems) || (field.maxItems !== undefined && value.length > field.maxItems) || (!field.custom && value.some((item) => !field.options.some((option) => option.value === item)))) throw new Error('Invalid OpenCode multiselect field');
    }
    // Branches above establish the exact published FormAnswer value union.
    if (typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean' || (Array.isArray(value) && value.every((item): item is string => typeof item === 'string'))) Object.defineProperty(answer, field.key, { value, enumerable: true });
    else throw new Error('Invalid OpenCode form value');
  }
  return answer;
}
