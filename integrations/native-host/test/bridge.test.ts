import assert from 'node:assert/strict';
import test from 'node:test';
import { Ajv } from 'ajv';
import { parseCommand } from '../src/bridge.js';
import { QuestionRegistry } from '../src/questions.js';
import type { NativeQuestion } from '../src/types.js';

const permission: NativeQuestion = { id: 'sdk-request-42', kind: 'permission', title: 'Run tool?', message: 'Provider request', options: [{ id: 'allow_once', label: 'Allow once' }, { id: 'deny', label: 'Deny' }], questions: [], fields: {} };

test('packet parsing preserves exact target IDs and rejects malformed native commands', () => {
  const operation_id = '18446744073709551615';
  assert.deepEqual(parseCommand({ type: 'command', operation_id, activation: 'owner-generation', intent: { type: 'interrupt', turn_id: 'provider-turn-3' } }), { operation_id, activation: 'owner-generation', intent: { type: 'interrupt', turn_id: 'provider-turn-3' } });
  for (const operation_id of [-1, 0.5, Number.MAX_SAFE_INTEGER + 1, '18446744073709551616', '-1', '1.5', '01']) assert.throws(() => parseCommand({ type: 'command', operation_id, activation: 'a', intent: { type: 'prompt', text: 'hello' } }));
  assert.throws(() => parseCommand({ type: 'command', operation_id: '2', activation: 'a', intent: { type: 'prompt', text: 'x'.repeat(13000) } }));
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
