import assert from 'node:assert/strict';
import test from 'node:test';
import { Ajv } from 'ajv';
import { nativeUpdate, parseCommand } from '../src/bridge.js';
import { createAdapter as createPi } from '../src/pi.js';
import { createAdapter as createAmp } from '../src/amp.js';
import { QuestionRegistry } from '../src/questions.js';
import type { AdapterContext, NativeQuestion } from '../src/types.js';

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
    const context = { provider, command: ['node', '--model', 'unmapped'], runtime: '/nonexistent/no-provider-may-load' } as AdapterContext;
    await assert.rejects(create(context), /additional CLI arguments are unsupported/);
  }
});
