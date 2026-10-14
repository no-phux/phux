import assert from 'node:assert/strict';
import test from 'node:test';
import type { AgentSessionEvent, SessionEntry, SessionMessageEntry } from '@earendil-works/pi-coding-agent';
import { attachPiSession, createPiUI, type PiSession } from '../src/pi.js';
import type { AdapterContext, Json, NativeQuestion, NativeUpdate } from '../src/types.js';

function harness() {
  const updates: NativeUpdate[] = [];
  const identities: string[] = [];
  const turns: (string | undefined)[] = [];
  const connections: boolean[] = [];
  const questions = new Map<string, { question: NativeQuestion; answer(value: Json): void }>();
  const context: AdapterContext = {
    provider: 'pi', activation: 'activation', cwd: '/isolated/project', command: [], runtime: '/isolated/runtime', env: {},
    emit: update => { updates.push(update); },
    identity: id => { identities.push(id); },
    turn: id => { turns.push(id); },
    connection: connected => { connections.push(connected); },
    question: (question, signal) => new Promise<Json>((resolve, reject) => {
      const cancel = (): void => {
        questions.delete(question.id);
        reject(new Error('cancelled'));
      };
      if (signal?.aborted) { cancel(); return; }
      signal?.addEventListener('abort', cancel, { once: true });
      questions.set(question.id, { question, answer(value) {
        questions.delete(question.id);
        signal?.removeEventListener('abort', cancel);
        resolve(value);
      } });
    }),
    closeQuestions: () => {
      for (const pending of [...questions.values()]) pending.answer(null);
    },
  };
  return { context, updates, identities, turns, connections, questions };
}

function fakeSession() {
  let listener: ((event: AgentSessionEvent) => void) | undefined;
  let leaf: SessionEntry | undefined;
  let id = 'actual-pi-session';
  let aborts = 0;
  let disposals = 0;
  let unsubscribes = 0;
  let prompt: PiSession['prompt'] = async () => {};
  const session: PiSession = {
    sessionManager: { getSessionId: () => id, getLeafEntry: () => leaf },
    subscribe(callback) { listener = callback; return () => { unsubscribes++; listener = undefined; }; },
    prompt: (text, options) => prompt(text, options),
    async abort() { aborts++; },
    dispose() { disposals++; },
  };
  return {
    session,
    emit(event: AgentSessionEvent) { listener?.(event); },
    persist(entry: SessionEntry) { leaf = entry; },
    replaceSession(value: string) { id = value; },
    setPrompt(value: PiSession['prompt']) { prompt = value; },
    counts: () => ({ aborts, disposals, unsubscribes }),
  };
}

function userEntry(id = 'actual-user-entry'): SessionMessageEntry {
  return { type: 'message', id, parentId: null, timestamp: '2026-10-14T00:00:00Z',
    message: { role: 'user', content: 'hello', timestamp: 1 } };
}

function onlyQuestion(h: ReturnHarness) {
  assert.equal(h.questions.size, 1);
  const pending = [...h.questions.values()][0];
  assert.ok(pending);
  return pending;
}
// An explicit test data interface avoids deriving SDK types from function returns.
interface ReturnHarness { questions: Map<string, { question: NativeQuestion; answer(value: Json): void }> }

test('Pi UI preserves exact option strings and correlates simultaneous dialogs', async () => {
  const h = harness();
  const ui = createPiUI(h.context);
  const confirm = ui.confirm('Permission', 'Run the command?');
  const input = ui.input('Name', 'placeholder');
  assert.equal(h.questions.size, 2);
  const confirmQuestion = [...h.questions.values()].find(pending => pending.question.kind === 'confirm');
  const inputQuestion = [...h.questions.values()].find(pending => pending.question.kind === 'input');
  assert.ok(confirmQuestion);
  assert.ok(inputQuestion);
  assert.notEqual(confirmQuestion.question.id, inputQuestion.question.id);
  assert.deepEqual(inputQuestion.question.fields, { initial_value: '', placeholder: 'placeholder' });
  inputQuestion.answer({ text: 'native input' });
  confirmQuestion.answer({ confirmed: false });
  assert.equal(await confirm, false);
  assert.equal(await input, 'native input');
  const selected = ui.select('Choice', ['Allow once', 'Deny']);
  const question = onlyQuestion(h);
  assert.deepEqual(question.question.options, [
    { id: 'Allow once', label: 'Allow once' }, { id: 'Deny', label: 'Deny' },
  ]);
  question.answer({ option_id: 'Deny' });
  assert.equal(await selected, 'Deny');
  const editor = ui.editor('Edit', 'existing\ntext');
  assert.deepEqual(onlyQuestion(h).question.fields, { initial_value: 'existing\ntext' });
  onlyQuestion(h).answer({ text: 'revised\ntext' });
  assert.equal(await editor, 'revised\ntext');
});

test('Pi UI fails closed for invalid answers, cancellation, timeout and custom terminal UI', async () => {
  const h = harness();
  const ui = createPiUI(h.context);
  const wrong = ui.select('Permission', ['Deny', 'Allow']);
  onlyQuestion(h).answer({ option_id: 'yes' });
  await assert.rejects(wrong, /Invalid Pi selection/);
  const controller = new AbortController();
  const cancelled = ui.confirm('Permission', 'Run?', { signal: controller.signal });
  controller.abort();
  assert.equal(await cancelled, false);
  assert.equal(h.questions.size, 0);
  const timedOut = ui.input('Input', '', { timeout: 0 });
  assert.equal(await timedOut, undefined);
  const neverOpened = ui.select('Choice', ['a'], { signal: controller.signal });
  assert.equal(await neverOpened, undefined);
  assert.equal(h.questions.size, 0);
  const dismissedEditor = ui.editor('Edit', 'original');
  onlyQuestion(h).answer(null);
  assert.equal(await dismissedEditor, undefined);
  const explicitCancel = ui.confirm('Permission', 'Run?');
  const cancelledId = onlyQuestion(h).question.id;
  onlyQuestion(h).answer({ cancelled: true });
  assert.equal(await explicitCancel, false);
  assert.ok(h.updates.some(update => update.type === 'question_closed' &&
    update.id === cancelledId && update.reason === 'Pi UI dialog cancelled'));
  let componentCalled = false;
  await assert.rejects(ui.custom(() => { componentCalled = true; throw new Error('must not execute'); }), /terminal-only custom/);
  assert.equal(componentCalled, false);
  assert.throws(() => ui.setWidget('unsafe', () => { throw new Error('must not execute'); }), /terminal-only component widget/);
  assert.throws(() => ui.onTerminalInput(() => undefined), /terminal-only onTerminalInput/);
});

test('Pi prompt receipt does not await SDK preflight questions or model completion', async () => {
  const h = harness();
  const sdk = fakeSession();
  const ui = createPiUI(h.context);
  let complete: (() => void) | undefined;
  sdk.setPrompt(async (_text, options) => {
    const confirmed = await ui.confirm('Preflight', 'Continue?');
    assert.equal(confirmed, true);
    options?.preflightResult?.('started');
    await new Promise<void>(resolve => { complete = resolve; });
  });
  const adapter = attachPiSession(h.context, sdk.session);
  const receipt = await adapter.prompt('hello', '17');
  assert.equal(receipt.disposition, 'queued');
  assert.match(receipt.detail ?? '', /not provider admission/);
  onlyQuestion(h).answer({ confirmed: true });
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.ok(h.updates.some(update => update.type === 'notice' && update.text === 'Pi operation 17: SDK preflight started.'));
  assert.ok(complete);
  complete();
  await adapter.close();
  assert.deepEqual(sdk.counts(), { aborts: 1, disposals: 1, unsubscribes: 1 });
  assert.deepEqual(h.connections, [false]);
});

test('Pi maps persisted input IDs, tool IDs and actual settled lifecycle without invented turn IDs', async () => {
  const h = harness();
  const sdk = fakeSession();
  const adapter = attachPiSession(h.context, sdk.session);
  assert.deepEqual(h.identities, ['actual-pi-session']);
  sdk.emit({ type: 'agent_start' });
  sdk.emit({ type: 'turn_start' });
  assert.deepEqual(h.turns, []);
  const entry = userEntry();
  sdk.emit({ type: 'message_end', message: entry.message });
  // Official 1.1.0 persists synchronously AFTER notifying public listeners.
  sdk.persist(entry);
  await new Promise<void>(resolve => queueMicrotask(resolve));
  assert.deepEqual(h.turns, ['actual-user-entry']);
  assert.ok(h.updates.some(update => update.type === 'transcript' && update.entry.id === entry.id && update.entry.role === 'user'));
  sdk.emit({ type: 'tool_execution_start', toolCallId: 'provider-tool-id', toolName: 'bash', args: { command: 'pwd' } });
  sdk.emit({ type: 'tool_execution_end', toolCallId: 'provider-tool-id', toolName: 'bash', result: {
    content: [{ type: 'text', text: 'private tool output' }],
  }, isError: false });
  const toolEntries = h.updates.filter(update => update.type === 'transcript' && update.entry.role === 'tool');
  assert.equal(toolEntries.length, 2);
  assert.ok(toolEntries.every(update => update.type === 'transcript' && update.entry.id === 'provider-tool-id' && update.entry.tool?.call_id === 'provider-tool-id'));
  const last = toolEntries[1];
  assert.ok(last?.type === 'transcript');
  assert.equal(last.entry.tool?.status, 'ok');
  assert.equal(last.entry.tool?.output, '', 'default transcript must not leak tool output');
  assert.equal((await adapter.interrupt('wrong-id')).disposition, 'refused');
  assert.equal((await adapter.interrupt(entry.id)).disposition, 'completed');
  assert.equal(sdk.counts().aborts, 1);
  sdk.emit({ type: 'agent_end', messages: [], willRetry: true });
  assert.equal(h.turns.at(-1), entry.id, 'retryable agent_end is not settlement');
  sdk.emit({ type: 'agent_settled', aborted: true });
  assert.equal(h.turns.at(-1), undefined);
  assert.equal((await adapter.interrupt(entry.id)).disposition, 'refused');
  assert.equal(h.connections.length, 0, 'normal Pi settlement is not source loss');
  await adapter.close();
  await adapter.close();
  assert.equal(sdk.counts().disposals, 1);
});

test('Pi rejects asynchronously without dumping SDK credentials or blocking subsequent commands', async () => {
  const h = harness();
  const sdk = fakeSession();
  sdk.setPrompt(async () => { throw new Error('secret-api-key'); });
  const adapter = attachPiSession(h.context, sdk.session);
  assert.equal((await adapter.prompt('hello', '1')).disposition, 'queued');
  await new Promise<void>(resolve => setImmediate(resolve));
  assert.ok(h.updates.some(update => update.type === 'notice' && update.level === 'error'));
  assert.equal(JSON.stringify(h.updates).includes('secret-api-key'), false);
  sdk.replaceSession('actual-replacement-id');
  sdk.emit({ type: 'agent_start' });
  assert.equal(h.identities.at(-1), 'actual-replacement-id');
  assert.equal(adapter.providerSessionId, 'actual-replacement-id');
  await adapter.close();
  assert.equal((await adapter.prompt('after close', '2')).disposition, 'refused');
});

test('Pi streaming response and tool updates keep actual IDs and bounded full output', async () => {
  const h = harness();
  h.context.env.PHUX_AGENT_TRANSCRIPT = 'full';
  const sdk = fakeSession();
  const adapter = attachPiSession(h.context, sdk.session);
  const entry: SessionMessageEntry = {
    type: 'message', id: 'persisted-assistant-entry', parentId: null, timestamp: '2026-10-14T00:00:00Z',
    message: {
      role: 'assistant', content: [{ type: 'text', text: 'answer' }],
      api: 'anthropic-messages', provider: 'anthropic', model: 'configured-model',
      responseId: 'actual-provider-response', timestamp: 1, stopReason: 'stop',
      usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
    },
  };
  sdk.emit({ type: 'message_start', message: entry.message });
  sdk.emit({ type: 'message_end', message: entry.message });
  sdk.persist(entry);
  await new Promise<void>(resolve => queueMicrotask(resolve));
  const messages = h.updates.filter(update => update.type === 'transcript' && update.entry.role === 'assistant');
  assert.equal(messages.length, 2);
  assert.ok(messages.every(update => update.type === 'transcript' && update.entry.id === 'actual-provider-response'));
  const finalMessage = messages[1];
  assert.ok(finalMessage?.type === 'transcript' && finalMessage.entry.final);
  sdk.emit({ type: 'tool_execution_start', toolName: 'bash', toolCallId: 'actual-call', args: { command: 'pwd' } });
  sdk.emit({ type: 'tool_execution_update', toolName: 'bash', toolCallId: 'actual-call', args: { command: 'pwd' },
    partialResult: { content: [{ type: 'text', text: 'partial' }] } });
  const partial = h.updates.at(-1);
  assert.ok(partial?.type === 'transcript');
  assert.equal(partial.entry.id, 'actual-call');
  assert.equal(partial.entry.final, false);
  assert.equal(partial.entry.tool?.output, 'partial');
  sdk.emit({ type: 'tool_execution_end', toolName: 'bash', toolCallId: 'actual-call', isError: true,
    result: { content: [{ type: 'text', text: 'x'.repeat(10_000) + 'tail' }] } });
  const finalTool = h.updates.at(-1);
  assert.ok(finalTool?.type === 'transcript');
  assert.equal(finalTool.entry.tool?.status, 'error');
  assert.ok((finalTool.entry.tool?.output.length ?? 0) <= 4096);
  assert.ok(finalTool.entry.tool?.output.endsWith('tail'));
  await adapter.close();
});
