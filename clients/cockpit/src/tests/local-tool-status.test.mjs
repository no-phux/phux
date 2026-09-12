import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialModel, update } from '../core.ts';
import { localToolReply } from '../local-tools.ts';
const bytes = value => new TextEncoder().encode(value);
const text = value => new TextDecoder().decode(value);
const token = new Uint8Array([42, 0, 0, 0, 0, 0, 0, 0]);
const field = value => [value.length, 0, ...value];
const reply = (phase, id = 9, capture = token) => new Uint8Array([1, phase, id, 0, 0, 0, ...capture, ...field(bytes('/config')), ...field(bytes('receipt'))]);
const step = (model, msg) => { const value = update(model, msg); return Array.isArray(value) ? value : [value, null]; };
const effects = cmd => !cmd ? [] : cmd.op === 'batch' ? cmd.cmds.flatMap(effects) : [cmd];
const request = cmd => effects(cmd).find(effect => effect.name === 'cockpit.local-tools');
function queued() {
  let [model] = step(initialModel()[0], { kind: 'config_edit' });
  [model] = step(model, { kind: 'local_tool_loaded', body: reply(0, 0) });
  return step(model, { kind: 'local_tool_loaded', body: reply(1) });
}

test('local-tool codec retains operation identity and accepts placed and unknown receipts', () => {
  for (const phase of [1, 2, 4, 5]) assert.equal(localToolReply(reply(phase)).operation, 9);
});

test('a queued tool starts status reads with its own capture after its dialog closes', () => {
  const [model, cmd] = queued();
  assert.equal(model.hostOpen, false);
  assert.equal(model.toolOperationId, 9);
  assert.deepEqual(model.toolOperationToken, token);
  assert.equal(request(cmd).payload[1], 4);
  assert.deepEqual(request(cmd).payload.slice(2, 10), token);
});

test('status polling is single-flight and cannot follow a new Describe token', () => {
  let [model] = queued();
  const [unchanged, duplicate] = step(model, { kind: 'tool_status_tick', at: 0 });
  assert.equal(request(duplicate), undefined);
  [model] = step(unchanged, { kind: 'add_machine_open' });
  [model] = step(model, { kind: 'local_tool_loaded', body: reply(0, 0, new Uint8Array(8).fill(7)) });
  [model] = step(model, { kind: 'local_tool_status_loaded', body: reply(1) });
  const [, poll] = step(model, { kind: 'tool_status_tick', at: 500 });
  assert.equal(request(poll).payload[1], 4);
  assert.deepEqual(request(poll).payload.slice(2, 10), token);
});

test('only the matching completed operation clears queued and acknowledges its native receipt', () => {
  let [model] = queued();
  const [stale, ignored] = step(model, { kind: 'local_tool_status_loaded', body: reply(4, 8) });
  assert.equal(stale.toolQueued, true);
  assert.equal(request(ignored), undefined);
  const [done, ack] = step(model, { kind: 'local_tool_status_loaded', body: reply(4) });
  assert.equal(done.toolQueued, false);
  assert.equal(request(ack).payload[1], 5);
  assert.deepEqual(request(ack).payload.slice(2, 10), token);
  assert.equal(text(done.commandNotice), '');
});

test('failed and unknown outcomes remain visible and never resubmit the tool', () => {
  for (const phase of [2, 5]) {
    const [model] = queued();
    const [done, cmd] = step(model, { kind: 'local_tool_status_loaded', body: reply(phase) });
    assert.equal(done.toolQueued, false);
    assert.match(text(done.commandNotice), /This Mac/);
    assert.equal(request(cmd).payload[1], 5);
    const [, tick] = step(done, { kind: 'tool_status_tick', at: 1000 });
    assert.equal(request(tick), undefined);
  }
});

test('receipt acknowledgement retries preserve the outcome and release the next launch only on confirmation', () => {
  let [model] = queued();
  [model] = step(model, { kind: 'local_tool_status_loaded', body: reply(5) });
  const notice = model.commandNotice.slice();
  let cmd;
  [model, cmd] = step(model, { kind: 'local_tool_ack_failed', error: bytes('failed') });
  assert.deepEqual(model.commandNotice, notice);
  assert.equal(cmd.op, 'delay');
  [model, cmd] = step(model, { kind: 'tool_status_tick', at: 1000 });
  assert.equal(request(cmd).payload[1], 5);
  [model] = step(model, { kind: 'local_tool_acknowledged', body: reply(5) });
  assert.equal(model.toolOperationId, 0);
  assert.deepEqual(model.commandNotice, notice);
});

test('placed status does not erase a newer independent command notice', () => {
  let [model] = queued();
  const notice = bytes('Another command was refused');
  [model] = step({ ...model, commandNotice: notice }, { kind: 'local_tool_status_loaded', body: reply(4) });
  assert.deepEqual(model.commandNotice, notice);
});

test('Edit Configuration does not carry retained Add Machine fields into the editor request', () => {
  let [model] = step({ ...initialModel()[0], hostFriendlyName: bytes('Build machine') }, { kind: 'config_edit' });
  const [, cmd] = step(model, { kind: 'local_tool_loaded', body: reply(0, 0) });
  assert.deepEqual(request(cmd).payload.slice(10), new Uint8Array([0, 0]));
});
