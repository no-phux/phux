import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import test, { type TestContext } from 'node:test';
import { CODEX_VERSION, createAdapter, questionId } from '../src/codex.js';
import type { AdapterContext, Json, NativeQuestion, NativeUpdate } from '../src/types.js';
import type { ServerRequest } from '../src/codex-protocol/generated/ServerRequest.js';
import type { ServerNotification } from '../src/codex-protocol/generated/ServerNotification.js';
import type { Thread } from '../src/codex-protocol/generated/v2/Thread.js';
import type { ThreadStartResponse } from '../src/codex-protocol/generated/v2/ThreadStartResponse.js';
import type { Turn } from '../src/codex-protocol/generated/v2/Turn.js';
import type { McpServerElicitationRequestParams } from '../src/codex-protocol/generated/v2/McpServerElicitationRequestParams.js';
import protocolSource from '../src/codex-protocol/source.json';

const thread: Thread = {
  id: 'thread-exact', sessionId: 'tree-exact', forkedFromId: null, parentThreadId: null,
  preview: '', ephemeral: false, section: null, sectionEnteredAt: null, projectId: null,
  historyMode: 'legacy', modelProvider: 'openai', model: 'fixture', reasoningEffort: null,
  createdAt: 1, updatedAt: 1, recencyAt: null, status: { type: 'idle' }, path: null, cwd: '/tmp',
  cliVersion: CODEX_VERSION, originator: null, source: 'appServer', threadSource: null,
  agentNickname: null, agentRole: null, gitInfo: null, name: null, turns: [],
};
const threadResponse: ThreadStartResponse = {
  thread, model: 'fixture', modelProvider: 'openai', serviceTier: null, disabledPluginIds: [],
  cwd: '/tmp', instructionSources: [], approvalPolicy: 'on-request', approvalsReviewer: 'user',
  sandbox: { type: 'readOnly', networkAccess: false }, reasoningEffort: null,
};
const turn: Turn = {
  id: 'turn-exact', rootTurnId: null, items: [], itemsView: 'full', status: 'inProgress',
  error: null, startedAt: 1, completedAt: null, durationMs: null,
};
const commandPermission: ServerRequest = {
  method: 'item/commandExecution/requestApproval', id: 7,
  params: { kind: 'command', threadId: thread.id, turnId: turn.id, itemId: 'item-exact',
    startedAtMs: 1, environmentId: null, approvalId: 'approval-exact', command: 'echo fixture', cwd: '/tmp', reason: 'fixture permission' },
};

interface FixtureOptions {
  packets?: Array<ServerRequest | ServerNotification>;
  question?: (question: NativeQuestion, signal?: AbortSignal) => Promise<Json>;
  version?: string;
  resumeSessionId?: string;
  resolveOnInterrupt?: boolean;
  closeOnPrompt?: boolean;
  completionBeforeResponse?: boolean;
}

async function openFixture(t: TestContext, options: FixtureOptions = {}) {
  const updates: NativeUpdate[] = [];
  const identities: string[] = [];
  const turns: Array<string | undefined> = [];
  const connections: boolean[] = [];
  const questions: NativeQuestion[] = [];
  // A real stdio process exercises the adapter's framing, response correlation, validation,
  // and nonblocking UI path. It is a protocol fixture, never a Codex model invocation.
  const program = `
    const assert = require('node:assert/strict');
    const readline = require('node:readline');
    const settings = ${JSON.stringify({ ...options, question: undefined })};
    const thread = ${JSON.stringify(threadResponse)};
    const turn = ${JSON.stringify(turn)};
    const send = packet => process.stdout.write(JSON.stringify(packet) + '\\n');
    const notice = message => send({ method: 'warning', params: { threadId: thread.thread.id, message } });
    if (process.argv.includes('--version')) {
      process.stdout.write('codex-cli ' + (settings.version ?? '${CODEX_VERSION}') + '\\n');
    } else {
      assert.deepEqual(process.argv.slice(1), ['app-server', '--listen', 'stdio://']);
      let initialized = false;
      let initializationRequested = false;
      const lines = readline.createInterface({ input: process.stdin });
      lines.on('line', line => {
        const m = JSON.parse(line);
        if (m.method === 'initialize') {
          assert.equal(initializationRequested, false);
          initializationRequested = true;
          assert.equal(m.params.capabilities.requestAttestation, false);
          assert.equal(m.params.capabilities.explicitGatewayOauth, true);
          send({ id: m.id, result: { userAgent: 'codex-cli/${CODEX_VERSION}', codexHome: '/tmp/fixture-only', platformFamily: 'unix', platformOs: 'macos' } });
        } else if (m.method === 'initialized') {
          assert.equal(initializationRequested, true);
          initialized = true;
        } else if (m.method === 'thread/start' || m.method === 'thread/resume') {
          assert.equal(initialized, true);
          assert.equal(Object.hasOwn(m.params, 'approvalPolicy'), false);
          assert.equal(Object.hasOwn(m.params, 'sandbox'), false);
          assert.equal(Object.hasOwn(m.params, 'approvalsReviewer'), false);
          if (settings.resumeSessionId) {
            assert.equal(m.method, 'thread/resume');
            assert.equal(m.params.threadId, settings.resumeSessionId);
          } else assert.equal(m.method, 'thread/start');
          send({ id: m.id, result: { ...thread, collaborationMode: null, turnsBackwardsCursor: null, itemsBackwardsCursor: null } });
        } else if (m.method === 'turn/start') {
          assert.equal(m.params.threadId, thread.thread.id);
          assert.equal(m.params.input[0].type, 'text');
          assert.equal(Object.hasOwn(m.params, 'approvalPolicy'), false);
          assert.equal(Object.hasOwn(m.params, 'sandboxPolicy'), false);
          if (settings.closeOnPrompt) { process.exit(0); return; }
          send({ method: 'turn/started', params: { threadId: thread.thread.id, turn } });
          if (settings.completionBeforeResponse) send({ method: 'turn/completed', params: { threadId: thread.thread.id, turn: { ...turn, status: 'completed' } } });
          send({ id: m.id, result: { turn } });
          for (const packet of settings.packets ?? []) send(packet);
        } else if (m.method === 'turn/interrupt') {
          assert.equal(m.params.threadId, thread.thread.id);
          assert.equal(m.params.turnId, turn.id);
          send({ id: m.id, result: {} });
          if (settings.resolveOnInterrupt) send({ method: 'serverRequest/resolved', params: { threadId: thread.thread.id, requestId: 7 } });
          send({ method: 'turn/completed', params: { threadId: thread.thread.id, turn: { ...turn, status: 'interrupted' } } });
        } else if (Object.hasOwn(m, 'id') && !m.method) {
          notice('reply:' + JSON.stringify(m));
          send({ method: 'serverRequest/resolved', params: { threadId: thread.thread.id, requestId: m.id } });
        } else throw new Error('Unexpected client packet');
      });
    }
  `;
  const context: AdapterContext = {
    provider: 'codex', activation: 'activation-exact', cwd: '/tmp',
    command: [process.execPath, '-e', program, '--'], runtime: '/unused-private-runtime',
    env: { ...process.env, PHUX_AGENT_TRANSCRIPT: 'full' }, resumeSessionId: options.resumeSessionId,
    emit: (update) => updates.push(update), identity: (id) => identities.push(id),
    turn: (id) => turns.push(id), connection: (connected) => connections.push(connected),
    closeQuestions: () => {},
    question: async (question, signal) => {
      questions.push(question);
      if (!options.question) throw new Error('Fixture has no human answer');
      return options.question(question, signal);
    },
  };
  const adapter = await createAdapter(context);
  t.after(() => adapter.close());
  return { adapter, context, updates, identities, turns, questions, connections };
}

async function observedReply(updates: NativeUpdate[]): Promise<Record<string, unknown>> {
  for (let attempt = 0; attempt < 1000; attempt++) {
    const notice = updates.find((u) => u.type === 'notice' && u.text.startsWith('reply:'));
    if (notice?.type === 'notice') return JSON.parse(notice.text.slice('reply:'.length)) as Record<string, unknown>;
    await delay(5);
  }
  throw new Error('Fixture did not observe a provider reply');
}

test('protocol provenance and typed RPC question IDs remain exact', () => {
  assert.equal(protocolSource.version, CODEX_VERSION);
  assert.equal(protocolSource.commit, '092d3acd6bec3e3a14bdc7e7a2810ab628ab759d');
  assert.notEqual(questionId(7), questionId('7'));
  assert.equal(JSON.parse(questionId('rpc-exact')), 'rpc-exact');
});

test('initialize/initialized starts the exact native thread with inherited human permissions', async (t) => {
  const fixture = await openFixture(t);
  assert.deepEqual(fixture.identities, [thread.id]);
  assert.deepEqual(fixture.connections, [true]);
  assert.equal(fixture.adapter.providerSessionId, thread.id);
  assert.equal((await fixture.adapter.prompt('hello', '18446744073709551615')).disposition, 'admitted');
  assert.equal(fixture.turns.at(-1), turn.id);
  assert.equal((await fixture.adapter.interrupt('stale-turn')).disposition, 'refused');
  assert.equal((await fixture.adapter.interrupt(turn.id)).disposition, 'admitted');
});

test('resumes only the explicit actual thread ID', async (t) => {
  const fixture = await openFixture(t, { resumeSessionId: thread.id });
  assert.equal(fixture.adapter.providerSessionId, thread.id);
  assert.deepEqual(fixture.identities, [thread.id]);
});

test('holds permission callback without blocking interrupt or emitting a late resolved reply', { timeout: 10_000 }, async (t) => {
  const opened = Promise.withResolvers<void>();
  const fixture = await openFixture(t, {
    packets: [commandPermission], resolveOnInterrupt: true,
    question: async (_question, signal) => {
      const answer = Promise.withResolvers<Json>();
      signal?.addEventListener('abort', () => answer.reject(new Error('resolved')), { once: true });
      opened.resolve();
      return answer.promise;
    },
  });
  assert.equal((await fixture.adapter.prompt('needs permission', '2')).disposition, 'admitted');
  await opened.promise;
  assert.equal(fixture.questions[0].id, questionId(7));
  assert.deepEqual(fixture.questions[0].options.map((option) => option.id), ['accept', 'acceptForSession', 'decline', 'cancel']);
  assert.deepEqual(fixture.questions[0].fields, { rpc_request_id: 7, ...commandPermission.params });
  assert.equal((await fixture.adapter.interrupt(turn.id)).disposition, 'admitted');
  await delay(30);
  assert.equal(fixture.updates.some((u) => u.type === 'notice' && u.text.startsWith('reply:')), false);
  assert.equal(fixture.updates.some((u) => u.type === 'state' && u.state === 'done' && u.turn_id === turn.id), true);
});

test('maps the human-selected permission to the original numeric RPC ID without blanket approval', async (t) => {
  const fixture = await openFixture(t, { packets: [commandPermission], question: async () => ({ option_id: 'decline' }) });
  await fixture.adapter.prompt('permission', '3');
  assert.deepEqual(await observedReply(fixture.updates), { id: 7, result: { decision: 'decline' } });
});

test('maps exact request_user_input question IDs and provider option labels', async (t) => {
  const request: ServerRequest = {
    method: 'item/tool/requestUserInput', id: 'input-exact',
    params: { threadId: thread.id, turnId: turn.id, itemId: 'input-item', isBlocking: true, autoResolutionMs: null,
      questions: [{ id: 'choice-exact', header: 'Pick', question: 'Which?', isOther: true, isSecret: false,
        options: [{ label: 'Exact provider label', description: 'Provider description' }] }] },
  };
  const fixture = await openFixture(t, { packets: [request], question: async () => ({ answers: [{ id: 'choice-exact', selected_options: ['Exact provider label'] }] }) });
  await fixture.adapter.prompt('input', '4');
  assert.deepEqual(await observedReply(fixture.updates), { id: 'input-exact', result: { answers: { 'choice-exact': { answers: ['Exact provider label'] } } } });
  assert.equal(fixture.questions[0].questions[0].options[0].id, 'Exact provider label');
});

const form: McpServerElicitationRequestParams = {
  threadId: thread.id, turnId: turn.id, serverName: 'fixture-mcp', mode: 'form', _meta: null,
  message: 'Enter a valid value', requestedSchema: { type: 'object', required: ['count'], properties: { count: { type: 'integer', minimum: 2, maximum: 5 } } },
};

test('validates MCP form values against the actual provider schema before replying', async (t) => {
  const fixture = await openFixture(t, {
    packets: [{ method: 'mcpServer/elicitation/request', id: 'form-exact', params: form }],
    question: async () => ({ values: { count: 3 } }),
  });
  await fixture.adapter.prompt('form', '5');
  assert.deepEqual(await observedReply(fixture.updates), { id: 'form-exact', result: { action: 'accept', content: { count: 3 }, _meta: null } });
  assert.deepEqual(fixture.questions[0].fields, form.requestedSchema);
});

test('fails closed for invalid form values and unsupported URL elicitation', async (t) => {
  const invalid = await openFixture(t, { packets: [{ method: 'mcpServer/elicitation/request', id: 'bad-form', params: form }], question: async () => ({ values: { count: 1 } }) });
  await invalid.adapter.prompt('bad form', '6');
  const invalidReply = await observedReply(invalid.updates);
  assert.equal(invalidReply.id, 'bad-form');
  assert.equal(Object.hasOwn(invalidReply, 'result'), false);
  assert.equal(Object.hasOwn(invalidReply, 'error'), true);

  const url = await openFixture(t, { packets: [{ method: 'mcpServer/elicitation/request', id: 'url-exact', params: {
    threadId: thread.id, turnId: null, serverName: 'fixture', mode: 'url', _meta: null,
    message: 'Open external auth', url: 'https://example.invalid/', elicitationId: 'external-exact',
  } }] });
  await url.adapter.prompt('url', '7');
  const urlReply = await observedReply(url.updates);
  assert.equal(Object.hasOwn(urlReply, 'error'), true);
  assert.equal(url.questions.length, 0);
});

test('projects real streaming messages and structured tool results using phux transcript entries', async (t) => {
  const packets: ServerNotification[] = [
    { method: 'item/agentMessage/delta', params: { threadId: thread.id, turnId: turn.id, itemId: 'message-exact', delta: 'Hello 🌍' } },
    { method: 'item/completed', params: { threadId: thread.id, turnId: turn.id, item: {
      type: 'agentMessage', id: 'message-exact', text: 'Hello 🌍!', phase: null, memoryCitation: null, delivery: null, questions: null,
    } } },
    { method: 'item/started', params: { threadId: thread.id, turnId: turn.id, item: {
      type: 'commandExecution', id: 'tool-exact', pluginId: null, scriptPath: null, command: 'echo fixture', cwd: '/tmp',
      processId: null, source: 'agent', status: 'inProgress', commandActions: [], aggregatedOutput: null, exitCode: null, durationMs: null,
    } } },
    { method: 'item/commandExecution/outputDelta', params: { threadId: thread.id, turnId: turn.id, itemId: 'tool-exact', delta: 'streaming output' } },
    { method: 'item/completed', params: { threadId: thread.id, turnId: turn.id, item: {
      type: 'commandExecution', id: 'tool-exact', pluginId: null, scriptPath: null, command: 'echo fixture', cwd: '/tmp',
      processId: null, source: 'agent', status: 'completed', commandActions: [], aggregatedOutput: 'fixture output', exitCode: 0, durationMs: 1,
    } } },
  ];
  const fixture = await openFixture(t, { packets });
  await fixture.adapter.prompt('messages', '8');
  for (let n = 0; n < 1000 && !fixture.updates.some((u) => u.type === 'transcript' && u.entry.id === 'tool-exact' && u.entry.final); n++) await delay(5);
  const entries = fixture.updates.filter((u) => u.type === 'transcript').map((u) => u.entry);
  assert.equal(entries.some((e) => e.id === 'message-exact' && !e.final && e.text === 'Hello 🌍'), true);
  assert.equal(entries.some((e) => e.id === 'message-exact' && e.final && e.text === 'Hello 🌍!'), true);
  assert.equal(entries.some((e) => e.id === 'tool-exact' && !e.final && e.tool?.output === 'streaming output'), true);
  assert.deepEqual(entries.find((e) => e.id === 'tool-exact' && e.final)?.tool, { name: 'command', call_id: 'tool-exact', summary: 'echo fixture', status: 'ok', output: 'fixture output' });
});

test('does not reopen a completed turn when its turn/start response arrives later', async (t) => {
  const fixture = await openFixture(t, { completionBeforeResponse: true });
  assert.equal((await fixture.adapter.prompt('fast turn', '9')).disposition, 'admitted');
  assert.equal(fixture.turns.at(-1), undefined);
  assert.equal(fixture.updates.filter((u) => u.type === 'state').at(-1)?.state, 'done');
});

test('source loss marks connectivity false and admission unknown, never successful completion', async (t) => {
  const fixture = await openFixture(t, { closeOnPrompt: true });
  assert.equal((await fixture.adapter.prompt('ambiguous delivery', '10')).disposition, 'unknown');
  assert.equal(fixture.connections.at(-1), false);
  assert.equal(fixture.updates.some((u) => u.type === 'state' && u.state === 'done'), false);
  assert.equal((await fixture.adapter.prompt('do not retry', '11')).disposition, 'refused');
});

test('rejects an incompatible executable before starting a provider session', async (t) => {
  await assert.rejects(openFixture(t, { version: '0.161.0' }), /requires executable version 0\.162\.1/);
});

test('explicit cancellation sends official cancel results rather than permission defaults', async (t) => {
  const permission = await openFixture(t, { packets: [commandPermission], question: async () => ({ cancelled: true }) });
  await permission.adapter.prompt('cancel command', '12');
  assert.deepEqual(await observedReply(permission.updates), { id: 7, result: { decision: 'cancel' } });
  const elicitation = await openFixture(t, { packets: [{ method: 'mcpServer/elicitation/request', id: 'cancel-form', params: form }], question: async () => ({ cancelled: true }) });
  await elicitation.adapter.prompt('cancel form', '13');
  assert.deepEqual(await observedReply(elicitation.updates), { id: 'cancel-form', result: { action: 'cancel', content: null, _meta: null } });
});

test('permission profiles grant only the exact requested profile and explicit scope', async (t) => {
  const request: ServerRequest = {
    method: 'item/permissions/requestApproval', id: 'profile-exact',
    params: { threadId: thread.id, turnId: turn.id, itemId: 'permissions-item', environmentId: null,
      startedAtMs: 1, cwd: '/tmp', reason: 'Network requested', permissions: { network: { enabled: true }, fileSystem: null } },
  };
  const chosen = { permissions: { network: { enabled: true } }, scope: 'turn' };
  const fixture = await openFixture(t, { packets: [request], question: async () => ({ option_id: JSON.stringify(chosen) }) });
  await fixture.adapter.prompt('grant only requested network', '14');
  assert.deepEqual(await observedReply(fixture.updates), { id: 'profile-exact', result: chosen });
  assert.equal(fixture.questions[0].options.some((option) => option.id.includes('fileSystem')), false);
  for (let attempt = 0; attempt < 1000 && !fixture.updates.some((u) => u.type === 'question_closed'); attempt++) await delay(5);
  assert.equal(fixture.updates.some((u) => u.type === 'question_closed' && u.id === questionId('profile-exact') && u.reason === 'Codex resolved the server request'), true);
});

test('unknown provider permission option is refused instead of silently accepted', async (t) => {
  const fixture = await openFixture(t, { packets: [commandPermission], question: async () => ({ option_id: 'yes' }) });
  await fixture.adapter.prompt('invalid permission answer', '15');
  const reply = await observedReply(fixture.updates);
  assert.equal(Object.hasOwn(reply, 'error'), true);
  assert.equal(Object.hasOwn(reply, 'result'), false);
});

test('nonblocking input remains answerable after a turn ends until the server resolves its exact request', { timeout: 10_000 }, async (t) => {
  const opened = Promise.withResolvers<void>();
  const answer = Promise.withResolvers<Json>();
  const request: ServerRequest = {
    method: 'item/tool/requestUserInput', id: 'async-input-exact',
    params: { threadId: thread.id, turnId: turn.id, itemId: 'async-item', isBlocking: false, autoResolutionMs: null,
      questions: [{ id: 'free-input-exact', header: 'Input', question: 'Continue?', isOther: true, isSecret: false, options: null }] },
  };
  const fixture = await openFixture(t, {
    packets: [request, { method: 'turn/completed', params: { threadId: thread.id, turn: { ...turn, status: 'completed' } } }],
    question: async (_question, signal) => {
      signal?.addEventListener('abort', () => answer.reject(new Error('Question prematurely cancelled')), { once: true });
      opened.resolve();
      return answer.promise;
    },
  });
  await fixture.adapter.prompt('async question', '16');
  await opened.promise;
  assert.equal(fixture.updates.some((u) => u.type === 'question_closed'), false);
  answer.resolve({ answers: [{ id: 'free-input-exact', selected_options: [], custom_input: 'Exact response' }] });
  assert.deepEqual(await observedReply(fixture.updates), { id: 'async-input-exact', result: { answers: { 'free-input-exact': { answers: ['Exact response'] } } } });
});

test('correlates simultaneous numeric and string RPC IDs without aliasing their human answers', async (t) => {
  const fixture = await openFixture(t, {
    packets: [commandPermission, { ...commandPermission, id: '7' }],
    question: async (question) => ({ option_id: question.id === questionId(7) ? 'decline' : 'cancel' }),
  });
  await fixture.adapter.prompt('distinct exact IDs', '17');
  for (let attempt = 0; attempt < 1000 && fixture.updates.filter((u) => u.type === 'notice' && u.text.startsWith('reply:')).length < 2; attempt++) await delay(5);
  const replies = fixture.updates.flatMap((u) => u.type === 'notice' && u.text.startsWith('reply:') ? [JSON.parse(u.text.slice(6)) as Record<string, unknown>] : []);
  assert.deepEqual(replies.find((reply) => reply.id === 7), { id: 7, result: { decision: 'decline' } });
  assert.deepEqual(replies.find((reply) => reply.id === '7'), { id: '7', result: { decision: 'cancel' } });
  assert.equal(fixture.questions.length, 2);
});
