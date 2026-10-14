import assert from 'node:assert/strict';
import test from 'node:test';
import { setTimeout as delay } from 'node:timers/promises';
import { OpenCode } from '@opencode/client';
import type { FormDetail, FormField, PermissionRequest, SessionEventDurable, SessionInfo, SessionMessageInfo, V2Event } from '@opencode/client';
import { OpenCodeAdapter, formSchema, privateServiceLaunch, validateFormAnswer } from '../src/opencode.js';
import type { AdapterContext, Json, NativeQuestion, NativeUpdate } from '../src/types.js';

const sessionID = 'ses_provider_real';
const session: SessionInfo = {
  id: sessionID, projectID: 'project_real', cost: 0,
  tokens: { input: 0, output: 0, reasoning: 0, cache: { read: 0, write: 0 } },
  time: { created: 1, updated: 2 }, location: { directory: '/project' },
};
const permission: PermissionRequest = {
  id: 'perm_real', sessionID, action: 'shell', resources: ['git status'], save: ['git *'],
  source: { type: 'tool', messageID: 'msg_assistant', id: 'call_real' },
};
function execution(id: string, seq: number): SessionEventDurable {
  return { id, created: seq, type: 'session.execution.started', durable: { aggregateID: sessionID, seq, version: 1 }, data: { sessionID } };
}
function assistant(text: string, final = false): SessionMessageInfo {
  return { id: 'msg_assistant', time: { created: 3, ...(final ? { completed: 4 } : {}) }, type: 'assistant', agent: 'build', model: { id: 'actual-model', providerID: 'actual-provider' }, content: [
    { type: 'text', text },
    { type: 'tool', id: 'call_real', name: 'shell', state: { status: 'running', input: { command: 'git status' }, metadata: {} }, time: { created: 3 } },
  ] };
}

interface Call { method: string; path: string; body: unknown }
/** Official OpenCode.make + generated HTTP/SSE codecs, with only transport faked. */
class Server {
  calls: Call[] = [];
  messages: SessionMessageInfo[] = [];
  permissions: PermissionRequest[] = [];
  forms: FormDetail[] = [];
  logs: SessionEventDurable[] = [];
  running = false;
  interrupted = true;
  losePromptAck = false;
  permissionAck?: Promise<void>;
  subscriptions = 0;
  private feeds = new Set<ReadableStreamDefaultController<Uint8Array>>();
  private readonly encoder = new TextEncoder();

  emit(event: V2Event): void {
    for (const feed of this.feeds) feed.enqueue(this.encoder.encode(`data: ${JSON.stringify(event)}\n\n`));
  }

  disconnect(): void {
    for (const feed of this.feeds) feed.close();
    this.feeds.clear();
  }

  readonly fetch: typeof fetch = async (input, init) => {
    const url = new URL(input instanceof Request ? input.url : String(input));
    const method = init?.method ?? 'GET';
    const body: unknown = typeof init?.body === 'string' ? JSON.parse(init.body) : undefined;
    this.calls.push({ method, path: url.pathname, body });
    const json = (value: unknown) => new Response(JSON.stringify(value), { status: 200, headers: { 'content-type': 'application/json' } });
    const empty = () => new Response(null, { status: 204 });
    if (url.pathname === '/api/event') {
      this.subscriptions++;
      let feed: ReadableStreamDefaultController<Uint8Array>;
      const stream = new ReadableStream<Uint8Array>({
        start: (controller) => {
          feed = controller;
          this.feeds.add(controller);
          controller.enqueue(this.encoder.encode('data: {"id":"connection","type":"server.connected","data":{}}\n\n'));
          init?.signal?.addEventListener('abort', () => {
            if (this.feeds.delete(controller)) controller.close();
          }, { once: true });
        },
        cancel: () => { this.feeds.delete(feed); },
      });
      return new Response(stream, { headers: { 'content-type': 'text/event-stream' } });
    }
    if (url.pathname === `/api/experimental/session/${sessionID}/log`) {
      const after = Number(url.searchParams.get('after') ?? 0);
      const events = this.logs.filter((event) => event.durable.seq > after);
      return new Response([...events, { type: 'log.synced', aggregateID: sessionID }].map((event) => `data: ${JSON.stringify(event)}\n\n`).join(''), { headers: { 'content-type': 'text/event-stream' } });
    }
    if (url.pathname === '/api/session' && method === 'POST') return json({ data: session });
    if (url.pathname === '/api/session/active') return json({ data: this.running ? { [sessionID]: { type: 'running' } } : {} });
    if (url.pathname === `/api/session/${sessionID}`) return json({ data: session });
    if (url.pathname === `/api/session/${sessionID}/message`) return json({ data: this.messages, cursor: {} });
    if (url.pathname === `/api/session/${sessionID}/permission`) return json({ data: this.permissions });
    if (url.pathname === `/api/session/${sessionID}/form`) return json({ data: this.forms.filter((form) => form.state.status === 'pending') });
    if (url.pathname === `/api/session/${sessionID}/prompt`) {
      if (this.losePromptAck) throw new Error('Connection lost after provider accepted the prompt');
      return json({ data: { id: 'inbox_real', sessionID, time: { created: 5 }, type: 'user', payload: { text: 'prompt' }, delivery: 'queue' } });
    }
    if (url.pathname === `/api/session/${sessionID}/interrupt`) return json({ interrupted: this.interrupted });
    if (url.pathname === `/api/session/${sessionID}/permission/${permission.id}/reply`) {
      await this.permissionAck;
      this.permissions = [];
      return empty();
    }
    for (const form of this.forms) {
      if (url.pathname === `/api/session/${sessionID}/form/${form.id}` && method === 'GET') return json({ data: form });
      if (url.pathname === `/api/session/${sessionID}/form/${form.id}` && method === 'DELETE') { form.state = { status: 'cancelled' }; return empty(); }
      if (url.pathname === `/api/session/${sessionID}/form/${form.id}/reply`) { form.state = { status: 'answered', answer: {} }; return empty(); }
    }
    throw new Error(`Unexpected generated HTTP request ${method} ${url.pathname}`);
  };
}

function harness(resume = false): {
  context: AdapterContext; updates: NativeUpdate[]; turns: Array<string | undefined>; identities: string[]; connections: boolean[];
  questions: Map<string, { question: NativeQuestion; resolve: (answer: Json) => void }>;
} {
  const updates: NativeUpdate[] = [];
  const turns: Array<string | undefined> = [];
  const identities: string[] = [];
  const connections: boolean[] = [];
  const questions = new Map<string, { question: NativeQuestion; resolve: (answer: Json) => void }>();
  const context: AdapterContext = {
    provider: 'opencode', activation: 'activation', cwd: '/project', command: ['/exact/opencode'], runtime: '/private/runtime', env: {},
    ...(resume ? { resumeSessionId: sessionID } : {}),
    emit: (update) => { updates.push(update); }, identity: (id) => { identities.push(id); }, turn: (id) => { turns.push(id); },
    connection: (connected) => { connections.push(connected); },
    question: (question, signal) => {
      const pending = Promise.withResolvers<Json>();
      questions.set(question.id, { question, resolve: pending.resolve });
      updates.push({ type: 'question_opened', question });
      signal?.addEventListener('abort', () => { questions.delete(question.id); pending.reject(new Error('Question closed')); }, { once: true });
      return pending.promise;
    },
    closeQuestions: () => { questions.clear(); },
  };
  return { context, updates, turns, identities, connections, questions };
}

async function until(predicate: () => boolean): Promise<void> {
  const deadline = Date.now() + 3_000;
  while (!predicate()) {
    if (Date.now() > deadline) assert.fail('Expected native adapter observation did not arrive');
    await delay(5);
  }
}

test('private service launch uses exact executable, ephemeral loopback port and no user registration/config', () => {
  const { context } = harness();
  context.command = ['/exact/node', '/exact/opencode.js'];
  context.env = { XDG_STATE_HOME: '/user/state', OPENCODE_CONFIG_DIR: '/user/config', OPENCODE_CONFIG: '/user/opencode.json', PHUX_NATIVE_BINDING: 'secret', OPENCODE_PTY_HANDOFF: 'secret', PHUX_SOCKET: '/scoped/socket' };
  const launch = privateServiceLaunch(context, '/private/unique');
  assert.equal(launch.command, '/exact/node');
  assert.deepEqual(launch.args, ['/exact/opencode.js', 'serve', '--service', '--hostname', '127.0.0.1', '--port', '0']);
  assert.equal(launch.file, '/private/unique/state/opencode/service.json');
  assert.equal(launch.env.OPENCODE_CONFIG_DIR, '/private/unique/config');
  assert.equal(launch.env.OPENCODE_CONFIG, '/user/opencode.json');
  assert.equal(launch.env.PHUX_NATIVE_BINDING, undefined);
  assert.equal(launch.env.OPENCODE_PTY_HANDOFF, undefined);
  assert.equal(launch.env.PHUX_SOCKET, '/scoped/socket');
  assert.equal(launch.env.PHUX_AGENT_REPORTING, '0');
});

test('real V2 snapshots retain IDs/tool rows and permission promises never block event drain or fake acknowledgement', async (t) => {
  const server = new Server();
  server.messages = [assistant('first')]; server.permissions = [permission]; server.running = true; server.logs = [execution('execution_real', 1)];
  const h = harness();
  const adapter = new OpenCodeAdapter(h.context, OpenCode.make({ baseUrl: 'http://private.invalid', fetch: server.fetch }), async () => {});
  t.after(() => adapter.close());
  await adapter.start();
  await until(() => h.connections.includes(true));
  assert.deepEqual(h.identities, [sessionID]);
  assert.equal(h.turns.at(-1), 'execution_real');
  assert.deepEqual(server.calls.find((call) => call.path === '/api/session' && call.method === 'POST')?.body, { location: { directory: '/project' } });
  assert.ok(h.updates.some((update) => update.type === 'transcript' && update.entry.id === 'call_real' && update.entry.tool?.call_id === 'call_real' && update.entry.tool.status === 'running'));
  assert.deepEqual(h.questions.get(permission.id)?.question.options, [{ id: 'once', label: 'Allow once' }, { id: 'always', label: 'Always allow' }, { id: 'reject', label: 'Reject' }]);
  assert.equal(server.calls.some((call) => call.path.endsWith('/reply')), false, 'no permission default is sent');
  server.messages = [assistant('streaming while human decides')];
  server.emit({ id: 'event_updated', created: 8, type: 'session.step.streamed', durable: { aggregateID: sessionID, seq: 2, version: 1 }, data: { sessionID, assistantMessageID: 'msg_assistant' } });
  await until(() => h.updates.some((update) => update.type === 'transcript' && update.entry.text === 'streaming while human decides'));
  const ack = Promise.withResolvers<void>(); server.permissionAck = ack.promise;
  h.questions.get(permission.id)!.resolve({ option_id: 'once' });
  await until(() => server.calls.some((call) => call.path.endsWith('/permission/perm_real/reply')));
  assert.equal(h.updates.some((update) => update.type === 'question_closed' && update.id === permission.id), false);
  ack.resolve();
  await until(() => h.updates.some((update) => update.type === 'question_closed' && update.id === permission.id));
  assert.deepEqual(server.calls.find((call) => call.path.endsWith('/permission/perm_real/reply'))?.body, { decision: 'once' });
});

test('volatile reconnect snapshots same session, current forms/permissions/messages and execution without replaying prompt', async (t) => {
  const server = new Server(); const h = harness();
  server.permissions = [permission]; server.running = true; server.logs = [execution('execution_before', 1)];
  const adapter = new OpenCodeAdapter(h.context, OpenCode.make({ baseUrl: 'http://private.invalid', fetch: server.fetch }), async () => {});
  t.after(() => adapter.close()); await adapter.start(); await until(() => h.connections.includes(true));
  const receipt = await adapter.prompt('one prompt only', '18446744073709551615');
  assert.equal(receipt.disposition, 'queued'); assert.match(receipt.detail!, /inbox_real.*queue/);
  assert.match(receipt.detail!, /host operation=18446744073709551615/);
  server.disconnect();
  await until(() => h.connections.at(-1) === false);
  server.permissions = [];
  server.messages = [assistant('changed while disconnected', true)];
  server.logs.push(execution('execution_after', 2));
  server.forms = [{ id: 'form_real', sessionID, title: 'Choose deployment', fields: [{ key: 'target', type: 'string', required: true, options: [{ value: 'dev-id', label: 'Development' }] }], state: { status: 'pending' } }];
  await until(() => server.subscriptions >= 2 && h.connections.at(-1) === true && h.questions.has('form_real'));
  assert.equal(h.questions.has(permission.id), false);
  assert.equal(h.turns.at(-1), 'execution_after');
  assert.ok(h.updates.some((update) => update.type === 'transcript' && update.entry.id === 'msg_assistant' && update.entry.text === 'changed while disconnected' && update.entry.final));
  assert.equal(server.calls.filter((call) => call.method === 'POST' && call.path === '/api/session').length, 1);
  assert.equal(server.calls.filter((call) => call.path.endsWith('/prompt')).length, 1);
  assert.equal(adapter.providerSessionId, sessionID);
  const stale = await adapter.interrupt('execution_before');
  assert.equal(stale.disposition, 'refused');
  assert.equal(server.calls.some((call) => call.path.endsWith('/interrupt')), false);
  assert.equal((await adapter.interrupt('execution_after')).disposition, 'completed');
  server.interrupted = false;
  assert.equal((await adapter.interrupt('execution_after')).disposition, 'refused');
  h.questions.get('form_real')!.resolve({ values: { target: 'dev-id' } });
  await until(() => server.calls.some((call) => call.path.endsWith('/form/form_real/reply')));
  assert.deepEqual(server.calls.find((call) => call.path.endsWith('/form/form_real/reply'))?.body, { answer: { target: 'dev-id' } });
});

test('resume gets only the exact supplied session; uncertain mutation is unknown and never retried', async (t) => {
  const server = new Server(); const h = harness(true);
  const adapter = new OpenCodeAdapter(h.context, OpenCode.make({ baseUrl: 'http://private.invalid', fetch: server.fetch }), async () => {});
  t.after(() => adapter.close()); await adapter.start(); await until(() => h.connections.includes(true));
  assert.equal(server.calls.some((call) => call.method === 'POST' && call.path === '/api/session'), false);
  server.losePromptAck = true;
  assert.equal((await adapter.prompt('do not retry', '7')).disposition, 'unknown');
  assert.equal(server.calls.filter((call) => call.path.endsWith('/prompt')).length, 1);
  adapter.serviceExited();
  assert.equal(h.connections.at(-1), false);
  assert.equal((await adapter.prompt('cannot send', '8')).disposition, 'refused');
  assert.equal(server.calls.filter((call) => call.path.endsWith('/prompt')).length, 1);
});

test('form schema preserves provider option IDs/labels and rejects invalid answers rather than coercing defaults', () => {
  const fields: FormField[] = [
    { key: 'choice', type: 'string', required: true, options: [{ value: 'opaque-id', label: 'Visible label' }] },
    { key: 'count', type: 'integer', minimum: 2, maximum: 5 },
    { key: 'many', type: 'multiselect', options: [{ value: 'a', label: 'A' }], minItems: 1, maxItems: 2 },
    { key: 'ok', type: 'boolean' },
  ];
  assert.deepEqual(formSchema(fields), {
    type: 'object', properties: {
      choice: { type: 'string', title: 'choice', oneOf: [{ const: 'opaque-id', title: 'Visible label' }] },
      count: { type: 'integer', title: 'count', minimum: 2, maximum: 5 },
      many: { type: 'array', title: 'many', items: { type: 'string', oneOf: [{ const: 'a', title: 'A' }] }, minItems: 1, maxItems: 2 },
      ok: { type: 'boolean', title: 'ok' },
    }, required: ['choice'], additionalProperties: false,
  });
  assert.deepEqual(validateFormAnswer(fields, { values: { choice: 'opaque-id', count: 3, many: ['a'], ok: false } }), { choice: 'opaque-id', count: 3, many: ['a'], ok: false });
  const invalidAnswers: Json[] = [{ choice: 'Visible label' }, { choice: 'opaque-id', count: 1 }, { choice: 'opaque-id', count: 2.1 }, { choice: 'opaque-id', ok: 'true' }, { choice: 'opaque-id', many: ['unknown'] }, { choice: 'opaque-id', unknown: true }, {}];
  for (const values of invalidAnswers) {
    assert.throws(() => validateFormAnswer(fields, { values }));
  }
  assert.throws(() => formSchema([{ key: 'browser', type: 'external', url: 'https://provider.invalid/auth' }]));
  assert.throws(() => formSchema([{ key: 'secret', type: 'string', hidden: true }]));
  assert.throws(() => formSchema([{ key: 'conditional', type: 'string', when: [{ key: 'ok', op: 'eq', value: true }] }]));
});

test('unsupported provider UI is explicitly cancelled, never answered or approved', async (t) => {
  const server = new Server(); const h = harness();
  server.forms = [{ id: 'external_form', sessionID, title: 'Browser action', fields: [{ key: 'external', type: 'external', url: 'https://provider.invalid/auth' }], state: { status: 'pending' } }];
  const adapter = new OpenCodeAdapter(h.context, OpenCode.make({ baseUrl: 'http://private.invalid', fetch: server.fetch }), async () => {});
  t.after(() => adapter.close()); await adapter.start();
  await until(() => server.calls.some((call) => call.method === 'DELETE' && call.path.endsWith('/form/external_form')));
  assert.equal(server.calls.some((call) => call.path.endsWith('/reply')), false);
  assert.equal(h.questions.has('external_form'), false);
  assert.ok(h.updates.some((update) => update.type === 'notice' && update.text.includes('unsupported')));
});

test('explicit human cancellation rejects permission and cancels form through actual V2 APIs', async (t) => {
  const server = new Server(); const h = harness();
  server.permissions = [permission];
  server.forms = [{ id: 'cancel_form', sessionID, title: 'Human question', fields: [{ key: 'value', type: 'string', required: true }], state: { status: 'pending' } }];
  const adapter = new OpenCodeAdapter(h.context, OpenCode.make({ baseUrl: 'http://private.invalid', fetch: server.fetch }), async () => {});
  t.after(() => adapter.close()); await adapter.start();
  await until(() => h.questions.has(permission.id) && h.questions.has('cancel_form'));
  h.questions.get(permission.id)!.resolve({ cancelled: true });
  h.questions.get('cancel_form')!.resolve({ cancelled: true });
  await until(() => h.updates.some((update) => update.type === 'question_closed' && update.id === permission.id) && h.updates.some((update) => update.type === 'question_closed' && update.id === 'cancel_form'));
  assert.deepEqual(server.calls.find((call) => call.path.endsWith('/permission/perm_real/reply'))?.body, { decision: 'reject' });
  assert.ok(server.calls.some((call) => call.method === 'DELETE' && call.path.endsWith('/form/cancel_form')));
  assert.equal(server.calls.some((call) => call.path.endsWith('/form/cancel_form/reply')), false);
});
