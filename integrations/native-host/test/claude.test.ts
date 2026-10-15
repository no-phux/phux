import assert from 'node:assert/strict';
import test from 'node:test';
import type { Options, Query, SDKMessage, SDKUserMessage, query as sdkQuery } from '@anthropic-ai/claude-agent-sdk/core';
import { createAdapter, claudeEnvironment } from '../src/claude.js';
import type { AdapterContext, Json, NativeCommandResult, NativeQuestion, NativeUpdate } from '../src/types.js';

class Events {
  private queued: SDKMessage[] = [];
  private waiting?: (event: IteratorResult<SDKMessage>) => void;
  private ended = false;

  send(event: SDKMessage): void {
    if (this.waiting) {
      const resolve = this.waiting;
      this.waiting = undefined;
      resolve({ done: false, value: event });
    } else this.queued.push(event);
  }

  finish(): void {
    this.ended = true;
    this.waiting?.({ done: true, value: undefined });
    this.waiting = undefined;
  }

  async *stream(): AsyncGenerator<SDKMessage, void> {
    for (;;) {
      const event = this.queued.shift();
      if (event) { yield event; continue; }
      if (this.ended) return;
      const { promise, resolve } = Promise.withResolvers<IteratorResult<SDKMessage>>();
      this.waiting = resolve;
      const next = await promise;
      if (next.done) return;
      yield next.value;
    }
  }
}

async function drained(): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  setImmediate(resolve);
  await promise;
}

/** SDK records only: no CLI, subprocess, model request, or terminal parsing. */
function fixture(version = '2.1.300') {
  const events = new Events();
  const updates: NativeUpdate[] = [];
  const results: NativeCommandResult[] = [];
  const identities: string[] = [];
  const turns: (string | undefined)[] = [];
  const connections: boolean[] = [];
  const closeReasons: string[] = [];
  const questions: { question: NativeQuestion; resolve: (value: Json) => void; reject: (error: Error) => void }[] = [];
  let options: Options | undefined;
  let input: AsyncIterable<SDKUserMessage> | undefined;
  let interruptCalls = 0;
  let closeCalls = 0;
  const context: AdapterContext = {
    provider: 'claude', activation: 'activation', cwd: '/tmp', command: [process.execPath], runtime: '/private/runtime',
    env: { PATH: '/usr/bin', PHUX_NATIVE_BINDING: 'must-not-reach-cli', PHUX_AGENT_TRANSCRIPT: 'full' },
    connection: (connected) => { connections.push(connected); },
    emit: (update) => { updates.push(update); },
    result: (result) => { results.push(result); },
    identity: (id) => { identities.push(id); },
    turn: (id) => { turns.push(id); },
    question: (question, signal) => {
      const { promise, resolve, reject } = Promise.withResolvers<Json>();
      const abort = () => reject(new Error('question cancelled'));
      if (signal?.aborted) abort();
      else signal?.addEventListener('abort', abort, { once: true });
      questions.push({ question, resolve, reject });
      return promise.finally(() => signal?.removeEventListener('abort', abort));
    },
    closeQuestions: (reason) => { closeReasons.push(reason); for (const question of questions) question.reject(new Error('source closed')); },
  };
  const factory: typeof sdkQuery = (params) => {
    assert.notEqual(typeof params.prompt, 'string', 'Claude must use live streaming input');
    input = params.prompt as AsyncIterable<SDKUserMessage>;
    options = params.options;
    // The stand-in implements only methods this module uses; it is not an SDK
    // substitute in production. Official types still define the factory seam.
    return Object.assign(events.stream(), {
      initializationResult: async () => ({ commands: [], agents: [], output_style: 'default', available_output_styles: [],
        models: [], account: {}, claude_code_version: version }),
      interrupt: async () => { interruptCalls++; return { still_queued: ['queued-provider-id'] }; },
      close: () => { closeCalls++; events.finish(); },
    }) as unknown as Query;
  };
  return {
    context, events, updates, results, identities, turns, connections, closeReasons, questions, factory,
    get options(): Options { assert.ok(options); return options; },
    get input(): AsyncIterable<SDKUserMessage> { assert.ok(input); return input; },
    get interruptCalls(): number { return interruptCalls; },
    get closeCalls(): number { return closeCalls; },
  };
}

function assistant(uuid: string, content: unknown[] = [], parent: string | null = null): SDKMessage {
  return { type: 'assistant', uuid: '00000000-0000-4000-8000-000000000001', session_id: 'actual-provider-session',
    parent_tool_use_id: parent, user_message_uuid: uuid, message: { id: 'actual-message-id', role: 'assistant', content } } as unknown as SDKMessage;
}

function result(uuid: string): SDKMessage {
  return { type: 'result', subtype: 'success', is_error: false, uuid: '00000000-0000-4000-8000-000000000002',
    session_id: 'actual-provider-session', user_message_uuid: uuid } as unknown as SDKMessage;
}

const permission = (requestId = 'actual-control-request') => ({ signal: new AbortController().signal, toolUseID: 'actual-tool-call', requestId });

test('subprocess environment removes host proof and injection while preserving delegated tool socket and provider auth', () => {
  const sanitized = claudeEnvironment({
    PHUX_NATIVE_BINDING: 'proof', PHUX_NATIVE_ACTIVATION: 'activation', PHUX_AGENT_HOST_SECRET: 'secret',
    PHUX_DELEGATED_CREDENTIAL_TOKEN: 'secret', NODE_OPTIONS: '--inspect', NODE_PATH: '/foreign/runtime',
    DEBUG_CLAUDE_AGENT_SDK: '1', DEBUG: '*', CLAUDECODE: '1',
    PHUX_SOCKET: '/delegated/socket', PHUX_TERMINAL_ID: 'guardian', ANTHROPIC_API_KEY: 'provider-credential',
  });
  assert.deepEqual(sanitized, { PHUX_SOCKET: '/delegated/socket', PHUX_TERMINAL_ID: 'guardian',
    ANTHROPIC_API_KEY: 'provider-credential', PHUX_AGENT_REPORTING: '0' });
});

test('startup rejects implicit executable discovery, CLI flags, JS entries, and unverified executable versions', async () => {
  const f = fixture();
  for (const command of [[], ['claude'], [process.execPath, '--dangerously-skip-permissions'], ['/tmp/cli.mjs']]) {
    await assert.rejects(createAdapter({ ...f.context, command }, f.factory), /Claude native mode/);
  }
  const mismatched = fixture('2.1.299');
  await assert.rejects(createAdapter(mismatched.context, mismatched.factory), /requires a verified Claude Code 2.1.300/);
  assert.equal(mismatched.closeCalls, 1);
  assert.equal(mismatched.connections.at(-1), false);
});

test('initializes the actual query with manual permissions, external executable and exact optional resume ID', async () => {
  const f = fixture();
  f.context.resumeSessionId = 'exact-resume-session';
  const adapter = await createAdapter(f.context, f.factory);
  assert.equal(f.options.pathToClaudeCodeExecutable, process.execPath);
  assert.equal(f.options.resume, 'exact-resume-session');
  assert.equal(f.options.permissionMode, 'default');
  assert.equal(f.options.allowDangerouslySkipPermissions, undefined);
  assert.equal(f.options.env?.PHUX_NATIVE_BINDING, undefined);
  assert.equal(f.options.env?.PHUX_AGENT_REPORTING, '0');
  assert.deepEqual(f.options.supportedDialogKinds, []);
  assert.deepEqual(await f.options.onUserDialog!({ dialogKind: 'unsupported', payload: {}, toolUseID: 'dialog-tool' },
    { signal: new AbortController().signal, requestId: 'unexpected-dialog' }), { behavior: 'cancelled' });
  assert.equal(adapter.providerSessionId, undefined, 'session identity cannot be fabricated from initialization');
  assert.deepEqual(f.connections, [true]);
  await adapter.close();
});

test('prompt queueing is not provider acceptance; real echoed IDs bind turns and input stays live after result', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  const input = f.input[Symbol.asyncIterator]();
  const receipt = await adapter.prompt('first turn', '1');
  assert.equal(receipt.disposition, 'queued');
  assert.match(receipt.detail!, /consumption not yet confirmed/);
  assert.equal(f.turns.length, 0);
  const first = (await input.next()).value!;
  assert.equal(first.origin?.kind, 'human');
  assert.ok(first.uuid);
  f.events.send(assistant(first.uuid!));
  await drained();
  assert.equal(adapter.providerSessionId, 'actual-provider-session');
  assert.deepEqual(f.identities, ['actual-provider-session']);
  assert.equal(f.turns.at(-1), first.uuid);
  assert.deepEqual(f.results.map(({ operation_id, disposition }) => ({ operation_id, disposition })),
    [{ operation_id: '1', disposition: 'admitted' }]);
  f.events.send(result(first.uuid!));
  await drained();
  assert.equal(f.turns.at(-1), undefined);
  assert.equal(f.results.at(-1)?.operation_id, '1');
  assert.equal(f.results.at(-1)?.disposition, 'completed');
  assert.equal((await adapter.prompt('follow-up turn', '2')).disposition, 'queued');
  const second = (await input.next()).value!;
  assert.equal(second.message.content, 'follow-up turn');
  assert.equal(second.session_id, 'actual-provider-session');
  f.events.send(assistant(second.uuid!));
  await drained();
  assert.equal((await adapter.interrupt(first.uuid!)).disposition, 'refused');
  assert.equal(f.interruptCalls, 0);
  const interrupted = await adapter.interrupt(second.uuid!);
  assert.equal(interrupted.disposition, 'completed');
  assert.match(interrupted.detail!, /1 queued prompts remain/);
  assert.equal(f.interruptCalls, 1);
  assert.equal(f.turns.at(-1), second.uuid, 'control acknowledgment is not turn termination');
  await adapter.close();
});

test('permission callbacks retain provider request/tool IDs and never block event draining or follow-up queueing', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  const waiting = f.options.canUseTool!('Bash', { command: 'pwd' }, permission());
  assert.equal(f.questions[0]!.question.id, 'actual-control-request');
  assert.deepEqual(f.questions[0]!.question.options.map((option) => option.id), ['deny', 'allow']);
  f.events.send(assistant('actual-turn', [{ type: 'text', text: 'still draining' }]));
  await drained();
  assert.ok(f.updates.some((update) => update.type === 'transcript' && update.entry.text === 'still draining'));
  assert.equal((await adapter.prompt('queued while blocked', '10')).disposition, 'queued');
  f.questions[0]!.resolve({ option_id: 'allow' });
  assert.deepEqual(await waiting, { behavior: 'allow', toolUseID: 'actual-tool-call' });
  assert.equal(f.updates.some((update) => update.type === 'question_closed'), false, 'callback return is not provider consumption');
  f.events.send({ type: 'tool_progress', tool_use_id: 'actual-tool-call', tool_name: 'Bash',
    parent_tool_use_id: null, elapsed_time_seconds: 1, uuid: '00000000-0000-4000-8000-000000000006', session_id: 'actual-provider-session' });
  await drained();
  assert.ok(f.updates.some((update) => update.type === 'question_closed' && update.id === 'actual-control-request' && update.reason.includes('tool outcome')));
  await adapter.close();
});

test('explicit denial, invalid permission answers and provider cancellation all fail closed', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  const responses: Json[] = [{ option_id: 'deny' }, { confirmed: true }, { option_id: 'always_allow' }, { cancelled: true }];
  for (const response of responses) {
    const pending = f.options.canUseTool!('Write', { file_path: '/tmp/example' }, permission(`permission-${f.questions.length}`));
    f.questions.at(-1)!.resolve(response);
    const resolved = await pending;
    assert.equal(resolved?.behavior, 'deny');
    assert.equal(resolved?.toolUseID, 'actual-tool-call');
  }
  const cancellation = new AbortController();
  const pending = f.options.canUseTool!('Bash', { command: 'pwd' }, { ...permission('cancelled-request'), signal: cancellation.signal });
  cancellation.abort();
  assert.equal((await pending)?.behavior, 'deny');
  await adapter.close();
});

test('AskUserQuestion maps exact question keys and labels, multi-select and free input without terminal UI', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  const toolInput = { questions: [
    { question: 'Which features?', header: 'Features', multiSelect: true,
      options: [{ label: 'Search', description: 'Find things' }, { label: 'Export', description: 'Save things' }] },
    { question: 'Which name?', header: 'Name', multiSelect: false,
      options: [{ label: 'Short', description: 'Concise' }, { label: 'Long', description: 'Verbose' }] },
  ] };
  const pending = f.options.canUseTool!('AskUserQuestion', toolInput, permission());
  assert.equal(f.questions[0]!.question.kind, 'ask');
  assert.equal(f.questions[0]!.question.questions[0]!.id, 'Which features?');
  f.questions[0]!.resolve({ answers: [
    { id: 'Which features?', selected_options: ['Search', 'Export'] },
    { id: 'Which name?', selected_options: [], custom_input: 'My exact choice' },
  ] });
  const resolved = await pending;
  assert.equal(resolved?.behavior, 'allow');
  if (resolved?.behavior === 'allow') {
    assert.deepEqual(resolved.updatedInput?.answers, Object.assign(Object.create(null) as Record<string, string>,
      { 'Which features?': 'Search, Export', 'Which name?': 'My exact choice' }));
  }
  const invalid = f.options.canUseTool!('AskUserQuestion', toolInput, permission('invalid-ask-request'));
  f.questions.at(-1)!.resolve({ answers: [{ id: 'Which features?', selected_options: ['invented'] }] });
  assert.equal((await invalid)?.behavior, 'deny');
  const previews = { questions: [{ ...toolInput.questions[0], options: [{ label: 'Search', description: '', preview: 'custom UI' }, { label: 'Export', description: '' }] }] };
  assert.equal((await f.options.canUseTool!('AskUserQuestion', previews, permission('unsupported-preview-request')))?.behavior, 'deny');
  await adapter.close();
});

test('MCP forms preserve original JSON schema and validate required fields, enums, formats and integer values', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  const requestedSchema = { type: 'object', required: ['count', 'email', 'choice'], additionalProperties: false,
    properties: { count: { type: 'integer', minimum: 1 }, email: { type: 'string', format: 'email' },
      choice: { type: 'string', enum: ['actual-id'], enumNames: ['Provider label'] } } };
  const request = { serverName: 'actual-mcp-server', message: 'Enter settings', mode: 'form' as const, requestedSchema };
  const pending = f.options.onElicitation!(request, { signal: new AbortController().signal, requestId: 'actual-elicitation-control-id' });
  assert.equal(f.questions.at(-1)!.question.id, 'actual-elicitation-control-id');
  assert.equal(f.questions.at(-1)!.question.kind, 'form');
  assert.deepEqual(f.questions.at(-1)!.question.fields, requestedSchema);
  f.questions.at(-1)!.resolve({ values: { count: 2, email: 'person@example.com', choice: 'actual-id' } });
  assert.deepEqual(await pending, { action: 'accept', content: { count: 2, email: 'person@example.com', choice: 'actual-id' } });
  assert.equal(f.updates.some((update) => update.type === 'question_closed'), false);
  f.events.send(assistant('actual-turn'));
  await drained();
  assert.equal(f.updates.some((update) => update.type === 'question_closed'), false, 'unrelated assistant output cannot consume a form');
  const invalidValues: Json[] = [
    { count: 1.5, email: 'person@example.com', choice: 'actual-id' },
    { count: 2, email: 'not-an-email', choice: 'actual-id' },
    { count: 2, email: 'person@example.com', choice: 'Provider label' },
    { count: 2, choice: 'actual-id' },
    { count: 2, email: 'person@example.com', choice: 'actual-id', extra: 'forbidden' },
  ];
  for (const values of invalidValues) {
    const invalid = f.options.onElicitation!(request, { signal: new AbortController().signal, requestId: `invalid-form-${f.questions.length}` });
    f.questions.at(-1)!.resolve({ values });
    assert.deepEqual(await invalid, { action: 'decline' });
  }
  const cancelled = f.options.onElicitation!(request, { signal: new AbortController().signal, requestId: 'cancelled-form' });
  f.questions.at(-1)!.resolve({ cancelled: true });
  assert.deepEqual(await cancelled, { action: 'cancel' });
  await adapter.close();
});

test('MCP URL flows, unknown schema keywords and nested unsupported fields never become implicit approvals', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  const opts = { signal: new AbortController().signal, requestId: 'unsupported-form-id' };
  assert.deepEqual(await f.options.onElicitation!({ serverName: 'server', message: 'Authenticate', mode: 'url',
    url: 'https://example.com/auth', elicitationId: 'actual-url-elicitation-id' }, opts), { action: 'cancel' });
  assert.deepEqual(await f.options.onElicitation!({ serverName: 'server', message: 'Nested', mode: 'form',
    requestedSchema: { type: 'object', properties: { nested: { type: 'object', properties: {} } } } }, opts), { action: 'cancel' });
  assert.deepEqual(await f.options.onElicitation!({ serverName: 'server', message: 'Unknown restriction', mode: 'form',
    requestedSchema: { type: 'object', properties: { name: { type: 'string', unsupportedAuthorization: true } } } }, opts), { action: 'cancel' });
  assert.equal(f.questions.length, 0);
  await adapter.close();
});

test('SDK streamed thinking/text and actual tool use/results replace transcript rows without raw state dumps', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  const stream = (event: unknown): SDKMessage => ({ type: 'stream_event', event, session_id: 'actual-provider-session',
    uuid: '00000000-0000-4000-8000-000000000003', parent_tool_use_id: null, user_message_uuid: 'actual-turn' } as SDKMessage);
  f.events.send(stream({ type: 'message_start', message: { id: 'streamed-message' } }));
  f.events.send(stream({ type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: '' } }));
  f.events.send(stream({ type: 'content_block_delta', index: 0, delta: { type: 'thinking_delta', thinking: 'Reasoning' } }));
  f.events.send(stream({ type: 'content_block_stop', index: 0 }));
  f.events.send(stream({ type: 'content_block_start', index: 1, content_block: { type: 'text', text: '' } }));
  f.events.send(stream({ type: 'content_block_delta', index: 1, delta: { type: 'text_delta', text: 'Answer' } }));
  f.events.send(stream({ type: 'content_block_stop', index: 1 }));
  f.events.send(stream({ type: 'message_stop' }));
  f.events.send(assistant('actual-turn', [{ type: 'tool_use', id: 'actual-tool-use-id', name: 'Bash', input: { command: 'pwd' } }]));
  f.events.send({ type: 'user', uuid: '00000000-0000-4000-8000-000000000004', session_id: 'actual-provider-session',
    parent_tool_use_id: null, message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'actual-tool-use-id',
      content: [{ type: 'text', text: '/tmp' }], is_error: false }] } });
  await drained();
  const entries = f.updates.flatMap((update) => update.type === 'transcript' ? [update.entry] : []);
  assert.ok(entries.some((entry) => entry.id === 'streamed-message:0' && entry.role === 'thinking' && entry.text === 'Reasoning' && entry.final));
  assert.ok(entries.some((entry) => entry.id === 'streamed-message:1' && entry.role === 'assistant' && entry.text === 'Answer' && entry.final));
  assert.ok(entries.some((entry) => entry.id === 'actual-tool-use-id' && entry.tool?.status === 'running' && entry.tool.summary === 'pwd'));
  assert.ok(entries.some((entry) => entry.id === 'actual-tool-use-id' && entry.tool?.status === 'ok' && entry.tool.output === '/tmp'));
  await adapter.close();
});

test('source close cancels unanswered provider questions, disconnects and refuses further input', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  const waiting = f.options.canUseTool!('Write', { file_path: '/tmp/example' }, permission());
  f.events.finish();
  await drained();
  assert.equal((await waiting)?.behavior, 'deny');
  assert.equal(f.connections.at(-1), false);
  assert.equal((await adapter.prompt('must not execute', '99')).disposition, 'refused');
  assert.equal((await adapter.interrupt('stale-turn')).disposition, 'refused');
  await adapter.close();
});

test('unconsumed prompt backlog is bounded and admission never invents provider consumption', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  for (let operation = 1; operation <= 64; operation++) assert.equal((await adapter.prompt('pending', String(operation))).disposition, 'queued');
  assert.equal((await adapter.prompt('overflow', '65')).disposition, 'refused');
  assert.equal(f.turns.length, 0);
  assert.equal(f.updates.some((update) => update.type === 'notice' && update.text.includes('consumed')), false);
  await adapter.close();
});

test('maximum u64 operation identity survives queueing and provider consumption without numeric conversion', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  assert.equal((await adapter.prompt('exact operation', '18446744073709551615')).disposition, 'queued');
  const message = (await f.input[Symbol.asyncIterator]().next()).value!;
  f.events.send(assistant(message.uuid!));
  await drained();
  assert.deepEqual(f.results.map(({ operation_id, disposition }) => ({ operation_id, disposition })),
    [{ operation_id: '18446744073709551615', disposition: 'admitted' }]);
  await adapter.close();
});

test('unsupported provider-required tool UI is denied rather than mistaken for permission approval', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  const providerUiOptions = { ...permission(), requiresUserInteraction: true };
  assert.equal((await f.options.canUseTool!('NewInteractiveTool', {}, providerUiOptions))?.behavior, 'deny');
  assert.equal(f.questions.length, 0);
  await adapter.close();
});

test('provider aborted assistant evidence finalizes interrupted streaming blocks as truncated', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  const stream = (event: unknown): SDKMessage => ({ type: 'stream_event', event, session_id: 'actual-provider-session',
    uuid: '00000000-0000-4000-8000-000000000007', parent_tool_use_id: null, user_message_uuid: 'actual-turn' } as SDKMessage);
  f.events.send(stream({ type: 'message_start', message: { id: 'interrupted-message' } }));
  f.events.send(stream({ type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } }));
  f.events.send(stream({ type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'Incomplete reply' } }));
  f.events.send({ ...assistant('actual-turn'), aborted: true,
    message: { id: 'interrupted-message', role: 'assistant', content: [] } } as unknown as SDKMessage);
  await drained();
  assert.ok(f.updates.some((update) => update.type === 'transcript' && update.entry.id === 'interrupted-message:0' &&
    update.entry.text === 'Incomplete reply' && update.entry.final && update.entry.truncated));
  await adapter.close();
});

test('batched consumed UUIDs retain exact operations until results and ignore duplicates, subagents and unrelated turns', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  const input = f.input[Symbol.asyncIterator]();
  await adapter.prompt('first private input', '18446744073709551615');
  await adapter.prompt('second private input', '2');
  await adapter.prompt('still queued private input', '3');
  const first = (await input.next()).value!;
  const second = (await input.next()).value!;
  const queued = (await input.next()).value!;
  f.events.send(assistant(first.uuid!, [], 'subagent-tool'));
  f.events.send(assistant('unrelated-uuid'));
  f.events.send(result('unrelated-uuid'));
  await drained();
  assert.deepEqual(f.results, []);
  const batch = { ...assistant(second.uuid!), user_message_uuids: [first.uuid!, second.uuid!] } as SDKMessage;
  f.events.send(batch);
  f.events.send(batch);
  await drained();
  assert.deepEqual(f.results.map(({ operation_id, disposition }) => [operation_id, disposition]),
    [['18446744073709551615', 'admitted'], ['2', 'admitted']]);
  const completion = { ...result(second.uuid!), user_message_uuids: [first.uuid!, second.uuid!] } as SDKMessage;
  f.events.send(completion);
  f.events.send(completion);
  f.events.send(batch);
  await drained();
  assert.deepEqual(f.results.map(({ operation_id, disposition }) => [operation_id, disposition]),
    [['18446744073709551615', 'admitted'], ['2', 'admitted'],
      ['18446744073709551615', 'completed'], ['2', 'completed']]);
  f.events.send({ ...result(queued.uuid!), is_error: true, subtype: 'error_during_execution',
    errors: ['provider-private-diagnostics'] } as SDKMessage);
  await drained();
  assert.equal(f.results.at(-1)?.operation_id, '3');
  assert.equal(f.results.at(-1)?.disposition, 'completed');
  assert.match(f.results.at(-1)!.detail!, /error_during_execution/);
  assert.equal(f.results.filter((receipt) => receipt.operation_id === '3').length, 1, 'a delivery-error result is not invented admission');
  assert.doesNotMatch(JSON.stringify(f.results), /private input|provider-private-diagnostics/);
  await adapter.close();
  assert.equal(f.results.length, 5, 'source closure must not overwrite terminal results');
});

test('source closure reports exact queued and admitted operations unknown without resending input', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  await adapter.prompt('admitted', '41');
  await adapter.prompt('queued', '42');
  const first = (await f.input[Symbol.asyncIterator]().next()).value!;
  f.events.send(assistant(first.uuid!));
  await drained();
  f.events.finish();
  await drained();
  assert.deepEqual(f.results.map(({ operation_id, disposition }) => [operation_id, disposition]),
    [['41', 'admitted'], ['41', 'unknown'], ['42', 'unknown']]);
  assert.equal((await adapter.prompt('do not retry', '43')).disposition, 'refused');
  await adapter.close();
});

test('concurrent submitted forms survive unrelated output and turn results until their own cancellation or source closure', async () => {
  const f = fixture();
  const adapter = await createAdapter(f.context, f.factory);
  const schema = { type: 'object', properties: { value: { type: 'string' } }, required: ['value'] };
  const request = { serverName: 'server', message: 'Settings', mode: 'form' as const, requestedSchema: schema };
  const firstSignal = new AbortController();
  const secondSignal = new AbortController();
  const first = f.options.onElicitation!(request, { signal: firstSignal.signal, requestId: 'form-one' });
  const second = f.options.onElicitation!(request, { signal: secondSignal.signal, requestId: 'form-two' });
  f.questions[0]!.resolve({ values: { value: 'first' } });
  f.questions[1]!.resolve({ values: { value: 'second' } });
  assert.equal((await first)?.action, 'accept');
  assert.equal((await second)?.action, 'accept');
  const tool = f.options.canUseTool!('Read', { path: '/tmp/example' }, permission('other-tool-request'));
  f.questions[2]!.resolve({ option_id: 'allow' });
  await tool;
  f.events.send(assistant('other-turn'));
  f.events.send({ type: 'stream_event', event: { type: 'ping' }, parent_tool_use_id: null,
    uuid: '00000000-0000-4000-8000-000000000009', session_id: 'actual-provider-session' } as SDKMessage);
  f.events.send({ type: 'tool_progress', tool_use_id: 'actual-tool-call', tool_name: 'Read', parent_tool_use_id: null,
    elapsed_time_seconds: 1, uuid: '00000000-0000-4000-8000-000000000010', session_id: 'actual-provider-session' });
  await drained();
  assert.equal((await adapter.interrupt('other-turn')).disposition, 'completed');
  assert.deepEqual(f.closeReasons, [], 'interrupt acknowledgment is not cancellation of independent form requests');
  f.events.send(result('other-turn'));
  await drained();
  assert.deepEqual(f.updates.flatMap((update) => update.type === 'question_closed' ? [update.id] : []), ['other-tool-request']);
  firstSignal.abort();
  assert.deepEqual(f.updates.flatMap((update) => update.type === 'question_closed' ? [update.id] : []), ['other-tool-request', 'form-one']);
  f.events.send(result('yet-another-turn'));
  await drained();
  assert.equal(f.updates.some((update) => update.type === 'question_closed' && update.id === 'form-two'), false);
  await adapter.close();
  assert.ok(f.updates.some((update) => update.type === 'question_closed' && update.id === 'form-two' && update.reason.includes('cancelled')));
});
