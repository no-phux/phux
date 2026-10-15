import { createHash } from 'node:crypto';
import { keepHead, keepTail, transcriptData, transcriptMode, type TranscriptData, type TranscriptMode } from '../../runtime/src/transcript.js';
import { QuestionRegistry } from './questions.js';
import type { AdapterContext, AdapterModule, Json, NativeAdapter, NativeCapabilities, NativeCommand, NativeCommandResult, NativeIntent, NativeUpdate } from './types.js';

export interface Startup { provider: string; activation: string; cwd: string; runtime: string; command: string[]; resumeSessionId?: string }
export type WireUpdate = Exclude<NativeUpdate, { type: 'transcript' }> | { type: 'transcript'; entry: TranscriptData };
export type OutputPacket = { type: 'ready'; provider_session_id?: string; capabilities: NativeCapabilities } | { type: 'update'; update: WireUpdate } | { type: 'result'; result: NativeCommandResult } | { type: 'failed'; message: string };
const MAX_PACKET_BYTES = 16 * 1024;
const MAX_BUFFER_BYTES = 8 * 1024 * 1024;

/** Match Rust Sink::update, including the complete provider_raw data envelope. */
export function nativeUpdate(provider: string, update: NativeUpdate, mode: TranscriptMode): WireUpdate | undefined {
  if (update.type === 'view') return { ...update, view: { ...update.view, backend: 'node' } };
  if (update.type !== 'transcript') return update;
  if (mode === 'off') return undefined;
  const source = update.entry.tool && mode !== 'full'
    ? { ...update.entry, tool: { ...update.entry.tool, output: '' } } : update.entry;
  let data = transcriptData(provider, source);
  for (;;) {
    const wire = { type: 'transcript' as const, entry: data };
    const over = Buffer.byteLength(JSON.stringify(wire)) - 14 * 1024;
    if (over <= 0) return wire;
    const entry = data.entry;
    // Measure serialized JSON, not just text: quotes, backslashes and lone
    // surrogates expand on the wire. UTF-8 helpers preserve code points.
    if (entry.text.length) {
      data = { ...data, entry: { ...entry, truncated: true, text: keepHead(entry.text, Math.max(0, Buffer.byteLength(entry.text) - over)) } };
    } else if (entry.tool?.output.length) {
      data = { ...data, entry: { ...entry, truncated: true, tool: { ...entry.tool, output: keepTail(entry.tool.output, Math.max(0, Buffer.byteLength(entry.tool.output) - over)) } } };
    } else {
      throw new Error('Native transcript metadata exceeds update bounds');
    }
  }
}

export function parseCommand(value: unknown): NativeCommand | { type: 'shutdown' } {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) throw new Error('Packet must be an object');
  const packet = value as Record<string, unknown>;
  if (packet.type === 'shutdown') return { type: 'shutdown' };
  if (packet.type !== 'command' || typeof packet.operation_id !== 'string' || !/^(0|[1-9][0-9]{0,19})$/.test(packet.operation_id) || BigInt(packet.operation_id) > 18446744073709551615n || typeof packet.activation !== 'string' || packet.activation.length > 256 || packet.intent === null || typeof packet.intent !== 'object' || Array.isArray(packet.intent)) throw new Error('Invalid native command envelope');
  const input = packet.intent as Record<string, unknown>;
  let intent: NativeIntent;
  if (input.type === 'prompt' && typeof input.text === 'string' && input.text.trim().length > 0) intent = { type: 'prompt', text: input.text };
  else if (input.type === 'interrupt' && typeof input.turn_id === 'string' && input.turn_id.length > 0 && input.turn_id.length <= 256) intent = { type: 'interrupt', turn_id: input.turn_id };
  else if (input.type === 'answer' && typeof input.question_id === 'string' && input.question_id.length > 0 && input.question_id.length <= 256 && input.response !== undefined) intent = { type: 'answer', question_id: input.question_id, response: input.response as Json };
  else throw new Error('Invalid native intent');
  return { operation_id: packet.operation_id, activation: packet.activation, intent };
}

export async function runBridge(startup: Startup, writeProtocol: (packet: string) => boolean): Promise<void> {
  // Native form validation is loaded only after the private runtime hook exists.
  const { Ajv } = await import('ajv');
  const { default: addFormats } = await import('ajv-formats');
  const ajv = new Ajv({ strict: false, allErrors: false, validateFormats: true, ownProperties: true });
  addFormats(ajv);
  let adapter: NativeAdapter | undefined;
  let providerId: string | undefined;
  let turnId: string | undefined;
  let connected = true;
  let available = true;
  let closing: Promise<void> | undefined;
  let failed = false;
  let capabilities: NativeCapabilities = { prompt: false, interrupt: false, questions: false, follow_up: false };
  let bufferedBytes = 0;
  let writable = true;
  const output: string[] = [];
  const fatal = (message: string): void => {
    if (failed) return;
    failed = true;
    process.exitCode = 1;
    // Fixed diagnostics only: never serialize an SDK exception/raw credentials.
    writeProtocol(JSON.stringify({ type: 'failed', message }) + '\n');
    void shutdown();
  };
  const flush = (): void => {
    while (writable && output.length) {
      const line = output.shift()!;
      bufferedBytes -= Buffer.byteLength(line);
      writable = writeProtocol(line);
    }
  };
  const send = (packet: OutputPacket): void => {
    if (failed) return;
    const line = JSON.stringify(packet) + '\n';
    const bytes = Buffer.byteLength(line);
    if (bytes > MAX_PACKET_BYTES || bufferedBytes + bytes > MAX_BUFFER_BYTES) { fatal('Native SDK output exceeded bounded protocol capacity; controls closed without retrying input'); return; }
    output.push(line);
    bufferedBytes += bytes;
    flush();
  };
  const publishView = (): void => send({ type: 'update', update: { type: 'view', view: { activation: startup.activation, backend: 'node', capabilities, connected: connected && available, ...(turnId ? { turn_id: turnId } : {}), ...(providerId ? { provider_session_id: providerId } : {}), questions: [...questions.pending.values()].map(item => item.question) } } });
  const emit = (update: NativeUpdate): void => {
    if (update.type === 'question_closed') questions.closed(update.id);
    const wire = nativeUpdate(startup.provider, update, transcriptMode(process.env.PHUX_AGENT_TRANSCRIPT));
    if (!wire) return;
    send({ type: 'update', update: wire });
    if (update.type === 'question_closed') publishView();
  };
  const questions = new QuestionRegistry(schema => ajv.compile(schema), question => {
    // An actual SDK UI callback proves only question support during startup;
    // do not advertise prompt/interrupt before SDK initialization completes.
    capabilities = { ...capabilities, questions: true };
    send({ type: 'update', update: { type: 'question_opened', question } });
    publishView();
  }, (id, reason) => { send({ type: 'update', update: { type: 'question_closed', id, reason } }); });
  const context: AdapterContext = {
    ...startup,
    env: { ...process.env },
    emit,
    connection(value) { available = value; publishView(); },
    identity(id) { if (!id || id.length > 256) throw new Error('Invalid provider session identity'); providerId = id; publishView(); },
    turn(id) { turnId = id; publishView(); },
    question: (question, signal) => questions.open(question, signal),
    closeQuestions(reason) { questions.closeAll(reason); publishView(); },
  };
  const ledger = new Map<string, { fingerprint: string; result: Promise<NativeCommandResult> }>();
  const dispatch = async (command: NativeCommand): Promise<NativeCommandResult> => {
    if (command.activation !== startup.activation) return { operation_id: command.operation_id, disposition: 'refused', detail: 'Native activation is stale' };
    if (!connected || !available) return { operation_id: command.operation_id, disposition: 'refused', detail: 'SDK source is unavailable' };
    const intent = command.intent;
    if (intent.type === 'prompt' && Buffer.byteLength(intent.text) > 12 * 1024) return { operation_id: command.operation_id, disposition: 'refused', detail: 'Prompt exceeds the native SDK 12288-byte supported limit; input was not dispatched' };
    if (intent.type === 'answer') {
      const answer = questions.answer(intent.question_id, intent.response);
      return { operation_id: command.operation_id, disposition: answer.accepted ? 'queued' : 'refused', detail: answer.detail };
    }
    if (!adapter) return { operation_id: command.operation_id, disposition: 'refused', detail: 'SDK initialization has not completed' };
    if (intent.type === 'interrupt' && (!capabilities.interrupt || turnId !== intent.turn_id)) return { operation_id: command.operation_id, disposition: 'refused', detail: 'Provider turn is stale or interruption is unsupported' };
    if (intent.type === 'prompt' && !capabilities.prompt) return { operation_id: command.operation_id, disposition: 'refused', detail: 'Provider prompts are unsupported' };
    try {
      const outcome = intent.type === 'prompt' ? await adapter.prompt(intent.text, command.operation_id) : await adapter.interrupt(intent.turn_id);
      return { operation_id: command.operation_id, ...outcome };
    } catch {
      // An SDK error after dispatch is ambiguous, never a safe automatic retry.
      return { operation_id: command.operation_id, disposition: 'unknown', detail: 'SDK command failed after dispatch; input was not retried' };
    }
  };
  const commandReceived = (command: NativeCommand): void => {
    const fingerprint = createHash('sha256').update(JSON.stringify(command)).digest('hex');
    const previous = ledger.get(command.operation_id);
    if (previous) {
      if (previous.fingerprint !== fingerprint) send({ type: 'result', result: { operation_id: command.operation_id, disposition: 'refused', detail: 'Operation ID was reused with a changed command' } });
      else void previous.result.then(result => send({ type: 'result', result }));
      return;
    }
    if (ledger.size >= 4096) { send({ type: 'result', result: { operation_id: command.operation_id, disposition: 'refused', detail: 'Native operation ledger is full; input was not dispatched' } }); return; }
    // Reserve before running SDK code, including synchronous callback reentry.
    let complete!: (result: NativeCommandResult) => void;
    const result = new Promise<NativeCommandResult>(resolve => { complete = resolve; });
    ledger.set(command.operation_id, { fingerprint, result });
    void dispatch(command).then(complete);
    void result.then(result => send({ type: 'result', result }));
  };
  function shutdown(): Promise<void> {
    if (closing) return closing;
    connected = false;
    questions.closeAll('host_closed');
    publishView();
    process.stdin.pause();
    closing = (async () => {
      await adapter?.close();
      process.stdin.removeListener('data', input);
      process.stdin.removeListener('end', ended);
      process.removeListener('SIGTERM', terminated);
      process.removeListener('SIGINT', terminated);
      flush();
    })();
    return closing;
  }
  let pending = Buffer.alloc(0);
  function input(chunk: Buffer): void {
    if (!connected) return;
    pending = Buffer.concat([pending, chunk]);
    for (;;) {
      const end = pending.indexOf(10);
      if (end < 0) break;
      if (end >= MAX_PACKET_BYTES) { fatal('Native SDK input packet exceeds protocol bounds'); return; }
      const line = pending.subarray(0, end).toString('utf8');
      pending = pending.subarray(end + 1);
      try {
        const packet = parseCommand(JSON.parse(line));
        if ('type' in packet) { void shutdown(); return; }
        commandReceived(packet);
      } catch { fatal('Malformed native SDK input packet; no terminal fallback is available'); return; }
    }
    if (pending.length >= MAX_PACKET_BYTES) fatal('Native SDK input packet exceeds protocol bounds');
  }
  const ended = (): void => { if (pending.length) fatal('Native SDK command stream ended mid-packet'); else void shutdown(); };
  const terminated = (): void => { void shutdown(); };
  process.stdout.on('drain', () => { writable = true; flush(); });
  process.stdout.on('error', () => { failed = true; process.exitCode = 1; void shutdown(); });
  process.stdin.on('data', input);
  process.stdin.on('end', ended);
  process.on('SIGTERM', terminated);
  process.on('SIGINT', terminated);
  try {
    if ((startup.provider === 'pi' || startup.provider === 'amp') && (startup.command.length !== 1 || startup.command[0] !== 'node')) {
      fatal('In-process Pi/Amp SDK requires command ["node"]; additional CLI arguments are unsupported and were not applied');
      return;
    }
    let module: AdapterModule;
    // Runtime-selected provider; each SDK is loaded only inside createAdapter.
    switch (startup.provider) {
      case 'pi': module = await import('./pi.js'); break;
      case 'claude': module = await import('./claude.js'); break;
      case 'opencode': module = await import('./opencode.js'); break;
      case 'codex': module = await import('./codex.js'); break;
      case 'amp': module = await import('./amp.js'); break;
      default: throw new Error('Unsupported native SDK provider');
    }
    adapter = await module.createAdapter(context);
    if (!connected) { await adapter.close(); return; }
    capabilities = adapter.capabilities;
    providerId = adapter.providerSessionId ?? providerId;
    send({ type: 'ready', ...(providerId ? { provider_session_id: providerId } : {}), capabilities });
    publishView();
  } catch { fatal('Native SDK initialization failed; verify isolated runtime assets and compatible provider executable on the executing host'); }
}
