import type { StreamMessage, UserInputMessage } from '@ampcode/sdk';
import { createRequire } from 'node:module';
import { readFile, access, open } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { constants } from 'node:fs';
import { summarizeArgs, transcriptMode } from '../../runtime/src/transcript.js';
import type { AdapterContext, NativeAdapter, TranscriptTool } from './types.js';

const SDK_VERSION = '0.1.0-20260918210405-g81edbf0';
const CLI_VERSION = '0.0.1792008042-gef0030';

/** Structured projection of the official SDK union; no terminal data enters it. */
export class AmpTranscript {
  sessionId: string | undefined;
  confirmed = false;
  readonly tools = new Map<string, TranscriptTool>();
  private userSequence = 0;
  constructor(private readonly context: AdapterContext) { this.sessionId = context.resumeSessionId; }
  emit(message: StreamMessage, requestId: string): void {
    const context = this.context;
    if (message.session_id) {
      if (this.sessionId !== undefined && this.sessionId !== message.session_id) throw new Error('Amp changed provider thread identity');
      this.sessionId = message.session_id;
      this.confirmed = true;
      context.identity(this.sessionId);
    }
    if (message.type === 'system') {
      context.emit({ type: 'state', state: 'working', turn_id: requestId });
      return;
    }
    if (message.type === 'assistant') {
      const text = message.message.content.filter(block => block.type === 'text').map(block => block.text).join('');
      if (text) context.emit({ type: 'transcript', entry: { id: message.message.id, role: 'assistant', text, final: true, truncated: false } });
      for (const block of message.message.content) {
        if (block.type !== 'tool_use') continue;
        const tool: TranscriptTool = { call_id: block.id, name: block.name, summary: summarizeArgs(block.input), status: 'running', output: '' };
        this.tools.set(block.id, tool);
        context.emit({ type: 'transcript', entry: { id: block.id, role: 'tool', text: '', tool, final: false, truncated: false } });
      }
      return;
    }
    if (message.type === 'user') {
      const text = message.message.content.filter(block => block.type === 'text').map(block => block.text).join('');
      if (text) context.emit({ type: 'transcript', entry: { id: `${requestId}:user:${++this.userSequence}`, role: 'user', text, final: true, truncated: false } });
      for (const block of message.message.content) {
        if (block.type !== 'tool_result') continue;
        const prior = this.tools.get(block.tool_use_id);
        const tool: TranscriptTool = { call_id: block.tool_use_id, name: prior?.name ?? 'tool', summary: prior?.summary ?? '', status: block.is_error ? 'error' : 'ok', output: transcriptMode(context.env.PHUX_AGENT_TRANSCRIPT) === 'full' ? block.content : '' };
        context.emit({ type: 'transcript', entry: { id: block.tool_use_id, role: 'tool', text: '', tool, final: true, truncated: false } });
        this.tools.delete(block.tool_use_id);
      }
      return;
    }
    for (const denial of message.permission_denials ?? []) {
      context.emit({ type: 'notice', level: 'warning', text: `Amp denied permission for ${denial.slice(0, 256)}. This SDK does not expose an interactive permission-reply API.` });
    }
    if (message.is_error) context.emit({ type: 'notice', level: 'error', text: 'Amp returned an execution error; provider diagnostics remain on the executing host.' });
    context.turn(undefined);
    context.emit({ type: 'state', state: 'done' });
  }
}

/** This SDK has no interactive approval callback. Default policy remains active;
 * permission_denials are reported, never converted into implicit approval. */
export async function createAdapter(context: AdapterContext): Promise<NativeAdapter> {
  // SDK loading must follow the isolated-runtime hook and sanitized process env.
  const { execute, createUserMessage } = await import('@ampcode/sdk');
  const require = createRequire(join(context.runtime, 'package.json'));
  const cliManifest = require.resolve('@ampcode/cli/package.json');
  const manifest: unknown = JSON.parse(await readFile(cliManifest, 'utf8'));
  if (typeof manifest !== 'object' || manifest === null || !('version' in manifest) || manifest.version !== CLI_VERSION) {
    throw new Error('Amp private runtime CLI version does not match the reviewed runtime lock');
  }
  // The SDK resolves its local package before AMP_CLI_PATH. Refuse missing local
  // assets rather than allowing its PATH/managed-install fallbacks to run.
  const binaryPath = join(dirname(cliManifest), 'bin', 'amp.exe');
  await access(binaryPath, constants.X_OK);
  const binary = await open(binaryPath, 'r');
  try {
    const header = Buffer.alloc(4);
    const { bytesRead } = await binary.read(header, 0, 4, 0);
    const magic = header.readUInt32BE(0);
    const native = magic === 0x7f454c46 || magic === 0xfeedface || magic === 0xfeedfacf || magic === 0xcefaedfe || magic === 0xcffaedfe || magic === 0xcafebabe || magic === 0xbebafeca || header.readUInt16BE(0) === 0x4d5a;
    if (bytesRead !== 4 || !native) throw new Error('Amp native runtime asset was not installed; refusing SDK wrapper fallback');
  } finally { await binary.close(); }
  const projection = new AmpTranscript(context);
  let active: { requestId: string; abort: AbortController; done: Promise<void> } | undefined;
  let closed = false;
  context.emit({ type: 'notice', level: 'info', text: `Amp SDK ${SDK_VERSION}: interactive permission replies and provider-confirmed input/cancel acknowledgments are unavailable. Cancellation terminates the current SDK execution; later prompts continue its actual thread ID.` });
  return {
    capabilities: { prompt: true, interrupt: true, questions: false, follow_up: true },
    get providerSessionId() { return projection.confirmed ? projection.sessionId : undefined; },
    async prompt(text, operationId) {
      if (closed) return { disposition: 'refused', detail: 'Amp adapter is closed' };
      if (active) return { disposition: 'refused', detail: 'Amp execution is active; wait for its result before continuing the same thread' };
      const requestId = `${context.activation}:${operationId}`;
      const abort = new AbortController();
      async function* input(): AsyncIterable<UserInputMessage> { yield createUserMessage(text, { requestId }); }
      const done = (async () => {
        try {
          let resultSeen = false;
          for await (const message of execute({ prompt: input(), options: { cwd: context.cwd, ...(projection.sessionId ? { continue: projection.sessionId } : {}), dangerouslyAllowAll: false }, signal: abort.signal })) {
            if (message.type === 'result') resultSeen = true;
            projection.emit(message, requestId);
          }
          if (!resultSeen) context.emit({ type: 'notice', level: 'warning', text: 'Amp SDK stream ended without a provider result; no command retry was attempted.' });
        } catch {
          context.emit({ type: 'notice', level: abort.signal.aborted ? 'warning' : 'error', text: abort.signal.aborted ? 'Amp SDK execution aborted locally; no provider semantic cancellation acknowledgment is available.' : 'Amp SDK execution failed; no input retry was attempted.' });
          // SDK throws after local teardown; that is not a provider semantic receipt.
          context.emit({ type: 'state', state: 'done' });
        } finally {
          active = undefined;
          context.turn(undefined);
          projection.tools.clear();
          if (!projection.confirmed) {
            context.emit({ type: 'notice', level: 'error', text: 'Amp ended before confirming a thread ID. Controls are unavailable rather than silently creating a replacement conversation.' });
            context.connection(false);
          }
        }
      })();
      active = { requestId, abort, done };
      context.turn(requestId); // This is the real requestId sent through createUserMessage, not a fabricated provider turn ID.
      return { disposition: 'queued', detail: 'Input queued to Amp SDK execute; this SDK exposes no provider admission receipt' };
    },
    async interrupt(turnId) {
      if (!active || active.requestId !== turnId) return { disposition: 'refused', detail: 'Amp input request is no longer active' };
      active.abort.abort();
      return { disposition: 'queued', detail: 'AbortSignal submitted to Amp SDK; process cancellation is not provider-confirmed turn interruption' };
    },
    async close() {
      closed = true;
      const current = active;
      current?.abort.abort();
      await current?.done;
    },
  };
}
