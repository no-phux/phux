import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { ExecuteOptions, StreamMessage, UserInputMessage, execute as sdkExecute } from '@ampcode/sdk';
import { AmpTranscript, createAdapter } from '../src/amp.js';
import type { AdapterContext, NativeCommandResult, NativeUpdate } from '../src/types.js';

test('Amp projects actual message/tool IDs and never creates approval callbacks', () => {
  const updates: NativeUpdate[] = [];
  const identities: string[] = [];
  const context: AdapterContext = { provider: 'amp', activation: 'a', cwd: '/tmp', command: [], runtime: '/tmp/private', env: {}, emit: update => { updates.push(update); }, result: () => {}, identity: id => { identities.push(id); }, turn: () => {}, connection: () => {}, question: async () => { throw new Error('Amp must not synthesize permission questions'); }, closeQuestions: () => {} };
  const projection = new AmpTranscript(context);
  projection.emit({ type: 'system', subtype: 'init', session_id: 'T-real', cwd: '/tmp', tools: ['Read'], mcp_servers: [] }, 'request-1');
  projection.emit({ type: 'assistant', session_id: 'T-real', parent_tool_use_id: null, message: { id: 'msg-real', role: 'assistant', type: 'message', model: 'provider-model', stop_reason: 'tool_use', stop_sequence: null, content: [{ type: 'text', text: 'Reading' }, { type: 'tool_use', id: 'tool-real', name: 'Read', input: { path: '/tmp/file' } }] } }, 'request-1');
  projection.emit({ type: 'user', session_id: 'T-real', parent_tool_use_id: null, message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'tool-real', content: 'private file content', is_error: false }] } }, 'request-1');
  projection.emit({ type: 'result', subtype: 'success', is_error: false, session_id: 'T-real', result: 'Finished', duration_ms: 2, num_turns: 1, permission_denials: ['Shell'] }, 'request-1');
  assert.equal(projection.sessionId, 'T-real');
  assert.ok(identities.every(id => id === 'T-real'));
  const transcripts = updates.flatMap(update => update.type === 'transcript' ? [update.entry] : []);
  assert.equal(transcripts[0].id, 'msg-real');
  assert.equal(transcripts[1].tool?.call_id, 'tool-real');
  assert.equal(transcripts[1].tool?.status, 'running');
  assert.equal(transcripts[2].tool?.call_id, 'tool-real');
  assert.equal(transcripts[2].tool?.status, 'ok');
  assert.equal(transcripts[2].tool?.output, '');
  assert.ok(updates.some(update => update.type === 'notice' && update.text.includes('does not expose')));
  assert.ok(!updates.some(update => update.type === 'question_opened'));
  assert.deepEqual(updates.at(-1), { type: 'state', state: 'done' });
  assert.throws(() => projection.emit({ type: 'system', subtype: 'init', session_id: 'T-different', cwd: '/tmp', tools: [], mcp_servers: [] }, 'request-1'), /identity/);
});

class Events {
  private readonly queued: StreamMessage[] = [];
  private waiting?: { resolve: (value: IteratorResult<StreamMessage>) => void; reject: (error: Error) => void };
  private ended = false;
  private error?: Error;
  send(message: StreamMessage): void {
    if (this.waiting) {
      this.waiting.resolve({ done: false, value: message });
      this.waiting = undefined;
    } else this.queued.push(message);
  }
  finish(): void {
    this.ended = true;
    this.waiting?.resolve({ done: true, value: undefined });
    this.waiting = undefined;
  }
  fail(error: Error): void {
    this.error = error;
    this.waiting?.reject(error);
    this.waiting = undefined;
  }
  async *stream(): AsyncGenerator<StreamMessage, void> {
    for (;;) {
      const message = this.queued.shift();
      if (message) { yield message; continue; }
      if (this.error) throw this.error;
      if (this.ended) return;
      const { promise, resolve, reject } = Promise.withResolvers<IteratorResult<StreamMessage>>();
      this.waiting = { resolve, reject };
      const next = await promise;
      if (next.done) return;
      yield next.value;
    }
  }
}

async function drained(): Promise<void> {
  await new Promise<void>((resolve) => setImmediate(resolve));
}

/** Native asset metadata is read, never launched. Only execute's typed stream is replaced. */
async function fixture(resumeSessionId?: string) {
  const runtime = await mkdtemp(join(tmpdir(), 'phux-amp-adapter-'));
  const cli = join(runtime, 'node_modules', '@ampcode', 'cli');
  await mkdir(join(cli, 'bin'), { recursive: true });
  await writeFile(join(cli, 'package.json'), JSON.stringify({ name: '@ampcode/cli', version: '0.0.1792008042-gef0030' }));
  await writeFile(join(cli, 'bin', 'amp.exe'), Buffer.from([0xcf, 0xfa, 0xed, 0xfe]), { mode: 0o700 });
  const updates: NativeUpdate[] = [];
  const results: NativeCommandResult[] = [];
  const turns: (string | undefined)[] = [];
  const connections: boolean[] = [];
  const calls: { params: ExecuteOptions; events: Events; input: UserInputMessage[] }[] = [];
  const context: AdapterContext = {
    provider: 'amp', activation: 'activation', cwd: '/tmp', command: ['node'], runtime, env: {},
    ...(resumeSessionId ? { resumeSessionId } : {}),
    emit: (update) => { updates.push(update); }, result: (result) => { results.push(result); },
    identity: () => {}, turn: (id) => { turns.push(id); }, connection: (connected) => { connections.push(connected); },
    question: async () => { throw new Error('Amp must not synthesize permission questions'); }, closeQuestions: () => {},
  };
  const execute: typeof sdkExecute = (params) => {
    const events = new Events();
    const call = { params, events, input: [] as UserInputMessage[] };
    calls.push(call);
    return (async function* () {
      assert.notEqual(typeof params.prompt, 'string');
      if (typeof params.prompt === 'string') throw new Error('Expected actual SDK streaming input');
      for await (const message of params.prompt) call.input.push(message);
      const abort = () => events.fail(new Error('private abort diagnostic'));
      params.signal?.addEventListener('abort', abort, { once: true });
      if (params.signal?.aborted) abort();
      try { yield* events.stream(); }
      finally { params.signal?.removeEventListener('abort', abort); }
    })();
  };
  const adapter = await createAdapter(context, execute);
  return { adapter, calls, results, updates, turns, connections,
    async cleanup() { await adapter.close(); await rm(runtime, { recursive: true, force: true }); } };
}

const init = (sessionId = 'T-real'): StreamMessage => ({
  type: 'system', subtype: 'init', session_id: sessionId, cwd: '/tmp', tools: [], mcp_servers: [],
});
const result = (sessionId = 'T-real'): StreamMessage => ({
  type: 'result', subtype: 'success', is_error: false, session_id: sessionId,
  result: 'private provider result', duration_ms: 2, num_turns: 1,
});

test('new Amp threads are private, correlated results finish exact operations, and continuation does not change visibility', async () => {
  const f = await fixture();
  try {
    assert.equal((await f.adapter.prompt('private input', '18446744073709551615')).disposition, 'queued');
    await drained();
    const first = f.calls[0]!;
    assert.equal(first.params.options?.visibility, 'private');
    assert.equal(first.params.options?.continue, undefined);
    assert.equal(first.params.options?.dangerouslyAllowAll, false);
    assert.equal(first.input[0]?.requestId, 'activation:18446744073709551615');
    assert.deepEqual(f.results, []);
    first.events.send(init());
    await drained();
    assert.deepEqual(f.results, [], 'thread initialization is not prompt admission');
    assert.equal((await f.adapter.prompt('concurrent input must not run', '2')).disposition, 'refused');
    assert.equal(f.calls.length, 1);
    first.events.send(result());
    first.events.send(result());
    first.events.fail(new Error('private trailing process diagnostic'));
    await drained();
    assert.deepEqual(f.results.map(({ operation_id, disposition }) => [operation_id, disposition]),
      [['18446744073709551615', 'completed']]);
    assert.doesNotMatch(JSON.stringify(f.results), /private/);
    assert.equal((await f.adapter.prompt('continue', '3')).disposition, 'queued');
    await drained();
    const second = f.calls[1]!;
    assert.equal(second.params.options?.continue, 'T-real');
    assert.equal(second.params.options?.visibility, undefined, 'resuming must preserve existing visibility');
    second.events.send({ type: 'result', subtype: 'error_during_execution', is_error: true, session_id: 'T-real',
      error: 'private provider diagnostics', duration_ms: 1, num_turns: 1 });
    second.events.finish();
    await drained();
    assert.deepEqual(f.results.map(({ operation_id, disposition }) => [operation_id, disposition]),
      [['18446744073709551615', 'completed'], ['3', 'completed']]);
    assert.match(f.results[1]!.detail!, /error_during_execution/);
    assert.doesNotMatch(JSON.stringify(f.results), /private/);
  } finally { await f.cleanup(); }
});

test('explicit Amp resume IDs never receive a visibility override', async () => {
  const f = await fixture('T-explicit-resume');
  try {
    await f.adapter.prompt('continue existing thread', '8');
    await drained();
    assert.equal(f.calls[0]!.params.options?.continue, 'T-explicit-resume');
    assert.equal(f.calls[0]!.params.options?.visibility, undefined);
    f.calls[0]!.events.send(result('T-explicit-resume'));
    f.calls[0]!.events.finish();
    await drained();
    assert.deepEqual(f.results.map(({ operation_id, disposition }) => [operation_id, disposition]), [['8', 'completed']]);
  } finally { await f.cleanup(); }
});

test('Amp local cancellation and stream loss remain unknown, never admitted or silently retried', async () => {
  const f = await fixture();
  try {
    await f.adapter.prompt('one', '1');
    f.calls[0]!.events.send(init());
    await drained();
    assert.equal((await f.adapter.interrupt('wrong-request')).disposition, 'refused');
    assert.equal(f.calls[0]!.params.signal?.aborted, false);
    assert.equal((await f.adapter.interrupt('activation:1')).disposition, 'queued');
    await drained();
    assert.deepEqual(f.results.map(({ operation_id, disposition }) => [operation_id, disposition]), [['1', 'unknown']]);
    assert.equal(f.calls.length, 1);
    await f.adapter.prompt('two', '2');
    f.calls[1]!.events.finish();
    await drained();
    assert.deepEqual(f.results.map(({ operation_id, disposition }) => [operation_id, disposition]), [['1', 'unknown'], ['2', 'unknown']]);
    assert.equal(f.calls[1]!.params.options?.continue, 'T-real');
    assert.doesNotMatch(JSON.stringify(f.results), /private abort diagnostic/);
  } finally { await f.cleanup(); }
});

test('Amp source failure before thread identity fences further prompts rather than creating a replacement thread', async () => {
  const f = await fixture();
  try {
    await f.adapter.prompt('uncertain input', '51');
    f.calls[0]!.events.fail(new Error('private failed dispatch diagnostic'));
    await drained();
    assert.deepEqual(f.results.map(({ operation_id, disposition }) => [operation_id, disposition]), [['51', 'unknown']]);
    assert.equal(f.connections.at(-1), false);
    assert.equal((await f.adapter.prompt('must not create a replacement', '52')).disposition, 'refused');
    assert.equal(f.calls.length, 1);
    assert.doesNotMatch(JSON.stringify(f.results), /private failed dispatch diagnostic/);
  } finally { await f.cleanup(); }
});
