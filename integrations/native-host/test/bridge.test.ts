import assert from 'node:assert/strict';
import { setImmediate as drained } from 'node:timers/promises';
import test from 'node:test';
import { Ajv } from 'ajv';
import { NativeOperationLedger, nativeUpdate, parseCommand } from '../src/bridge.js';
import { createAdapter as createPi } from '../src/pi.js';
import { createAdapter as createAmp } from '../src/amp.js';
import { QuestionRegistry } from '../src/questions.js';
import type { AdapterContext, NativeCommand, NativeCommandResult, NativeQuestion } from '../src/types.js';

const permission: NativeQuestion = { id: 'sdk-request-42', kind: 'permission', title: 'Run tool?', message: 'Provider request', options: [{ id: 'allow_once', label: 'Allow once' }, { id: 'deny', label: 'Deny' }], questions: [], fields: {} };

test('packet parsing preserves exact target IDs and rejects malformed native commands', () => {
  const operation_id = '18446744073709551615';
  assert.deepEqual(parseCommand({ type: 'command', operation_id, activation: 'owner-generation', intent: { type: 'interrupt', turn_id: 'provider-turn-3' } }), { operation_id, activation: 'owner-generation', intent: { type: 'interrupt', turn_id: 'provider-turn-3' } });
  for (const operation_id of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1, '18446744073709551616', '-1', '1.5', '01']) assert.throws(() => parseCommand({ type: 'command', operation_id, activation: 'a', intent: { type: 'prompt', text: 'hello' } }));
  const oversized = parseCommand({ type: 'command', operation_id: '2', activation: 'a', intent: { type: 'prompt', text: 'x'.repeat(13000) } });
  assert.ok(!('type' in oversized) && oversized.intent.type === 'prompt');
  assert.throws(() => parseCommand({ type: 'command', operation_id: '2', activation: 'a', intent: { type: 'interrupt', turn_id: '' } }));
  assert.throws(() => parseCommand({ type: 'paste', text: 'yes\n' }));
});

test('answer admission is single-responder, exact option ID, and not provider consumption', async () => {
  const ajv = new Ajv();
  const closed: string[] = [];
  const registry = new QuestionRegistry(schema => ajv.compile(schema), () => {}, id => { closed.push(id); });
  const pending = registry.open(permission);
  assert.equal(registry.answer(permission.id, { option_id: 'yes' }).accepted, false);
  assert.equal(registry.answer(permission.id, { option_id: 'allow_once' }).accepted, true);
  assert.equal(registry.answer(permission.id, { option_id: 'deny' }).accepted, false);
  assert.deepEqual(await pending, { option_id: 'allow_once' });
  assert.equal(registry.pending.has(permission.id), true);
  assert.deepEqual(closed, []);
  registry.closed(permission.id);
  assert.equal(registry.pending.has(permission.id), false);
});

test('native form uses actual provider schema and invalid values leave request pending', async () => {
  const ajv = new Ajv();
  const registry = new QuestionRegistry(schema => ajv.compile(schema), () => {}, () => {});
  const pending = registry.open({ ...permission, kind: 'form', options: [], fields: { type: 'object', properties: { count: { type: 'integer', minimum: 1 } }, required: ['count'], additionalProperties: false } });
  assert.equal(registry.answer(permission.id, { values: { count: '1' } }).accepted, false);
  assert.equal(registry.answer(permission.id, { values: { count: 1 } }).accepted, true);
  assert.deepEqual(await pending, { values: { count: 1 } });
  registry.closed(permission.id);
});

test('provider cancellation rejects waiting callback and closes only actual question ID', async () => {
  const ajv = new Ajv();
  const closed: string[] = [];
  const registry = new QuestionRegistry(schema => ajv.compile(schema), () => {}, id => { closed.push(id); });
  const abort = new AbortController();
  const pending = registry.open(permission, abort.signal);
  const rejected = assert.rejects(pending, /cancelled/);
  abort.abort();
  await rejected;
  assert.deepEqual(closed, [permission.id]);
  assert.equal(registry.answer(permission.id, { option_id: 'allow_once' }).accepted, false);
});

test('native transcript wire data retains the consumer schema and fits the host with escaped Unicode', () => {
  const text = ('🙂"\\\t\n\ud800').repeat(8000);
  const wire = nativeUpdate('pi', { type: 'transcript', entry: { id: 'provider-message', role: 'assistant', text, final: true, truncated: false } }, 'default');
  assert.ok(wire?.type === 'transcript');
  const retained = JSON.parse(JSON.stringify(wire));
  // NativeBinding projects this exact data as provider_raw.data; the transcript
  // consumer reads schema + entry, not an entry nested in another convention.
  assert.equal(retained.entry.schema, 'phux.transcript/v1');
  assert.equal(retained.entry.provider, 'pi');
  assert.equal(retained.entry.entry.id, 'provider-message');
  assert.equal(retained.entry.entry.role, 'assistant');
  assert.equal(retained.entry.entry.final, true);
  assert.equal(retained.entry.entry.truncated, true);
  assert.ok(text.startsWith(retained.entry.entry.text));
  assert.ok(Buffer.byteLength(JSON.stringify(wire)) <= 14 * 1024);
  assert.ok(Buffer.byteLength(JSON.stringify({ type: 'update', update: wire }) + '\n') <= 16 * 1024);
});

test('native transcript boundary applies output privacy before retaining the full envelope', () => {
  const update = { type: 'transcript' as const, entry: { id: 'call', role: 'tool' as const, text: '', final: true, truncated: false, tool: { name: 'Read', call_id: 'call', summary: '/file', status: 'ok' as const, output: 'private file content' } } };
  const normal = nativeUpdate('amp', update, 'default');
  const full = nativeUpdate('amp', update, 'full');
  assert.ok(normal?.type === 'transcript' && full?.type === 'transcript');
  assert.equal(normal.entry.entry.tool?.output, '');
  assert.equal(full.entry.entry.tool?.output, 'private file content');
  assert.equal(nativeUpdate('amp', update, 'off'), undefined);
});

test('in-process SDK descriptors reject unmapped CLI options before loading providers', async () => {
  for (const [provider, create] of [['pi', createPi], ['amp', createAmp]] as const) {
    const context: AdapterContext = {
      provider, activation: 'a', cwd: '/tmp', env: {}, command: ['node', '--model', 'unmapped'],
      runtime: '/nonexistent/no-provider-may-load', emit: () => {}, result: () => {},
      identity: () => {}, turn: () => {}, connection: () => {}, closeQuestions: () => {},
      question: async () => { throw new Error('Provider must reject unmapped CLI options before asking a question'); },
    };
    await assert.rejects(create(context), /additional CLI arguments are unsupported/);
  }
});

const nativePrompt = (operation_id: string, text = 'captured private draft'): NativeCommand => ({
  operation_id, activation: 'owner-generation', intent: { type: 'prompt', text },
});

test('operation ledger publishes async admission/completion and replays the latest result without redispatch', async () => {
  const packets: NativeCommandResult[] = [];
  let calls = 0;
  const ledger = new NativeOperationLedger(async command => {
    calls++;
    return { operation_id: command.operation_id, disposition: 'queued', detail: 'SDK input queue only' };
  }, result => packets.push(result));
  const command = nativePrompt('18446744073709551615');
  ledger.receive(command);
  await drained();
  assert.deepEqual(packets.map(result => result.disposition), ['queued']);
  ledger.result({ operation_id: command.operation_id, disposition: 'admitted', detail: 'SDK consumed the correlated input' });
  ledger.receive(command);
  assert.deepEqual(packets.map(result => result.disposition), ['queued', 'admitted', 'admitted']);
  ledger.result({ operation_id: command.operation_id, disposition: 'completed', detail: 'SDK correlated result received' });
  ledger.receive(command);
  ledger.receive({ ...command, intent: { type: 'prompt', text: 'changed private draft' } });
  assert.equal(packets.at(-1)?.disposition, 'refused');
  assert.match(packets.at(-1)?.detail ?? '', /changed command/);
  ledger.receive({ ...command, activation: 'different-generation' });
  assert.equal(packets.at(-1)?.disposition, 'refused');
  ledger.receive(command);
  assert.equal(packets.at(-1)?.disposition, 'completed', 'changed-payload refusal must not replace the original receipt');
  assert.equal(calls, 1);
  assert.ok(packets.every(result => result.operation_id === command.operation_id));
  assert.equal(JSON.stringify(packets).includes('private draft'), false);
});

test('operation reservation handles sync SDK reentry and pending duplicate requests without queued regression', async () => {
  const packets: NativeCommandResult[] = [];
  const initial = Promise.withResolvers<NativeCommandResult>();
  const command = nativePrompt('9007199254740993');
  let calls = 0;
  const ledger: NativeOperationLedger = new NativeOperationLedger(() => {
    calls++;
    ledger.receive(command);
    ledger.result({ operation_id: command.operation_id, disposition: 'admitted', detail: 'Synchronous consumed-input callback' });
    return initial.promise;
  }, result => packets.push(result));
  ledger.receive(command);
  assert.deepEqual(packets.map(result => result.disposition), ['admitted', 'admitted']);
  initial.resolve({ operation_id: command.operation_id, disposition: 'queued', detail: 'Late initial queued receipt' });
  await drained();
  ledger.result({ operation_id: command.operation_id, disposition: 'queued', detail: 'Late SDK queue callback' });
  ledger.receive(command);
  assert.deepEqual(packets.map(result => result.disposition), ['admitted', 'admitted', 'admitted']);
  assert.equal(calls, 1);
});

test('terminal receipts survive late callbacks and source close while unknown operations can gain factual evidence', async () => {
  const packets: NativeCommandResult[] = [];
  let calls = 0;
  const ledger = new NativeOperationLedger(async command => {
    calls++;
    return { operation_id: command.operation_id, disposition: 'queued' };
  }, result => packets.push(result));
  for (const id of ['1', '2', '3']) ledger.receive(nativePrompt(id));
  await drained();
  ledger.result({ operation_id: '1', disposition: 'completed', detail: 'Actual result' });
  ledger.result({ operation_id: '2', disposition: 'refused', detail: 'Actual rejection' });
  ledger.result({ operation_id: 'provider-uuid', disposition: 'completed' });
  ledger.close();
  ledger.result({ operation_id: '1', disposition: 'unknown', detail: 'Late disconnect' });
  ledger.result({ operation_id: '2', disposition: 'queued' });
  ledger.result({ operation_id: '3', disposition: 'queued' });
  for (const id of ['1', '2', '3']) ledger.receive(nativePrompt(id));
  assert.deepEqual(packets.slice(-3).map(result => [result.operation_id, result.disposition]),
    [['1', 'completed'], ['2', 'refused'], ['3', 'unknown']]);
  ledger.result({ operation_id: '3', disposition: 'completed', detail: 'Late actual result' });
  ledger.receive(nativePrompt('3'));
  assert.equal(packets.at(-1)?.disposition, 'completed');
  assert.equal(calls, 3);
  assert.ok(!packets.some(result => result.operation_id === 'provider-uuid'));
});

test('ambiguous dispatch never retries and concurrent receipts remain correlated', async () => {
  const packets: NativeCommandResult[] = [];
  const pending = new Map<string, { promise: Promise<NativeCommandResult>; resolve(result: NativeCommandResult): void; reject(reason: unknown): void }>();
  let calls = 0;
  const ledger = new NativeOperationLedger(command => {
    calls++;
    const task = Promise.withResolvers<NativeCommandResult>();
    pending.set(command.operation_id, task);
    return task.promise;
  }, result => packets.push(result));
  ledger.receive(nativePrompt('31'));
  ledger.receive(nativePrompt('32'));
  pending.get('32')!.resolve({ operation_id: '32', disposition: 'completed', detail: 'Second command completed first' });
  pending.get('31')!.reject(new Error('private-provider-credential'));
  await drained();
  assert.deepEqual(packets.map(result => [result.operation_id, result.disposition]), [['32', 'completed'], ['31', 'unknown']]);
  ledger.receive(nativePrompt('31'));
  assert.equal(packets.at(-1)?.disposition, 'unknown');
  ledger.result({ operation_id: '31', disposition: 'admitted', detail: 'Late correlated input evidence' });
  ledger.receive(nativePrompt('31'));
  assert.equal(packets.at(-1)?.disposition, 'admitted');
  assert.equal(calls, 2);
  assert.equal(JSON.stringify(packets).includes('private-provider-credential'), false);
});

test('bounded operation ledger refuses new IDs without evicting old deduplication evidence', async () => {
  const packets: NativeCommandResult[] = [];
  let calls = 0;
  const ledger = new NativeOperationLedger(async command => {
    calls++;
    return { operation_id: command.operation_id, disposition: 'completed' };
  }, result => packets.push(result));
  for (let id = 0; id < 4096; id++) ledger.receive(nativePrompt(String(id)));
  await drained();
  ledger.receive(nativePrompt('4096'));
  assert.equal(packets.at(-1)?.disposition, 'refused');
  assert.match(packets.at(-1)?.detail ?? '', /ledger is full/);
  ledger.receive(nativePrompt('0'));
  assert.equal(packets.at(-1)?.disposition, 'completed');
  assert.equal(calls, 4096);
});
