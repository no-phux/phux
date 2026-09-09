import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialModel, update } from '../core.ts';
import { enqueueTabCommand, receiveTabReceipt, initialTabCommands } from '../tab-commands.ts';
import { snapshot } from '../protocol.ts';

const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};
const target = id => {
  const bytes = new Uint8Array(22);
  bytes[0] = 1;
  new DataView(bytes.buffer).setUint32(18, id, true);
  return bytes;
};
const receipt = (request, applied = true) => {
  const bytes = new Uint8Array(27);
  bytes[0] = 1; bytes[1] = applied ? 1 : 2; bytes[2] = applied ? 0 : 2;
  bytes.set(request.subarray(2, 10), 3);
  return bytes;
};

test('queued selection captures exact targets and consumes only command receipts', () => {
  let model = initialModel()[0];
  const first = target(1), second = target(2);
  let command;
  [model, command] = step(model, { kind: 'select_target', target: first });
  assert.equal(command.name, 'cockpit.tab-command');
  assert.deepEqual(command.payload.subarray(10), first);
  let queued;
  [model, queued] = step(model, { kind: 'select_target', target: second });
  assert.equal(queued, null);
  second[18] = 99; // A later projection/buffer cannot retarget queued data.
  const event = new Uint8Array(18); event[0] = 1; event[1] = 1; event[2] = 9;
  [model] = step(model, { kind: 'engine_event', key: 0, state: 'data', bytes: event, droppedPending: 0, droppedTotal: 0 });
  assert.equal(model.tabCommands.queue.length, 2);
  [model, queued] = step(model, { kind: 'tab_command_completed', body: receipt(command.payload, false) });
  assert.equal(model.tabCommands.outcome, 3);
  assert.equal(queued.name, 'cockpit.tab-command');
  assert.equal(queued.payload[28], 2);
  assert.deepEqual(model.tabCommands.lastId, { hi: 0, lo: 1 });
  [model, command] = step(model, { kind: 'tab_command_completed', body: receipt(queued.payload) });
  assert.equal(command, null);
  assert.equal(model.tabCommands.queue.length, 0);
  assert.equal(model.tabCommands.outcome, 2);
  assert.deepEqual(model.tabCommands.lastId, { hi: 0, lo: 2 });
});

test('full FIFO refuses a new action without replacing any earlier selection', () => {
  let model = initialModel()[0];
  for (let i = 1; i <= 16; i++) [model] = step(model, { kind: 'select_target', target: target(i) });
  const before = model.tabCommands.queue;
  const [full, command] = step(model, { kind: 'select_target', target: target(17) });
  assert.equal(command, null);
  assert.equal(full.tabCommands.queue, before);
  assert.equal(full.tabCommands.outcome, 4);
  assert.match(new TextDecoder().decode(full.commandNotice), /queue full/);
  assert.deepEqual(before.map(entry => entry.bytes[28]), Array.from({ length: 16 }, (_, i) => i + 1));
});

test('unknown delivery never retries and explicitly cancels unsent selections', () => {
  for (const failure of ['transport', 'mismatch', 'malformed']) {
    let [model, command] = step(initialModel()[0], { kind: 'select_target', target: target(1) });
    [model] = step(model, { kind: 'select_target', target: target(2) });
    let body = receipt(command.payload);
    if (failure === 'mismatch') body[3] = 99;
    if (failure === 'malformed') body = body.subarray(0, 26);
    [model, command] = step(model, failure === 'transport'
      ? { kind: 'tab_command_failed', error: new Uint8Array() }
      : { kind: 'tab_command_completed', body });
    assert.equal(command, null);
    assert.equal(model.tabCommands.queue.length, 0);
    assert.equal(model.tabCommands.outcome, 5);
    assert.deepEqual(model.tabCommands.lastId, { hi: 0, lo: 1 });
    assert.match(new TextDecoder().decode(model.commandNotice), /unknown.*canceled/);
    [model, command] = step(model, { kind: 'select_target', target: target(3) });
    assert.equal(command.payload[2], 3);
  }
});

test('command IDs retain full u64 precision and fail closed on exhaustion', () => {
  let state = { ...initialTabCommands(), nextId: { hi: 0xabcdef01, lo: 0xffffffff } };
  let decision = enqueueTabCommand(state, target(1));
  assert.equal(new DataView(decision.request.buffer).getBigUint64(2, true), 0xabcdef01ffffffffn);
  assert.deepEqual(decision.state.nextId, { hi: 0xabcdef02, lo: 0 });
  decision = receiveTabReceipt(decision.state, receipt(decision.request));
  assert.deepEqual(decision.state.lastId, state.nextId);
  state = { ...initialTabCommands(), nextId: { hi: 0xffffffff, lo: 0xffffffff } };
  decision = enqueueTabCommand(state, target(1));
  const last = decision.request;
  decision = receiveTabReceipt(decision.state, receipt(last));
  decision = enqueueTabCommand(decision.state, target(2));
  assert.equal(decision.request.length, 0);
  assert.equal(decision.state.outcome, 6);
  assert.deepEqual(decision.state.lastId, state.nextId);
});

test('unknown command notice survives ordinary snapshots until a new explicit selection', () => {
  let [model] = step(initialModel()[0], { kind: 'select_target', target: target(1) });
  [model] = step(model, { kind: 'tab_command_failed', error: new Uint8Array() });
  const body = new Uint8Array(121);
  body[0] = 1; body[1] = 2; body[23] = 2; body[26] = 168; body[29] = 255;
  body[38] = 2; body[39] = 80;
  [model] = step(model, { kind: 'snapshot_loaded', body });
  assert.equal(model.engineConnected, true);
  assert.equal(model.tabCommands.outcome, 5);
  assert.match(new TextDecoder().decode(model.commandNotice), /unknown.*canceled/);
  [model] = step(model, { kind: 'select_target', target: target(2) });
  assert.equal(model.tabCommands.outcome, 1);
  assert.doesNotMatch(new TextDecoder().decode(model.commandNotice), /unknown/);
});

test('target context coexists with agent rows and unknown snapshot extension records', () => {
  const head = new Uint8Array(28);
  head[0] = 1; head[1] = 2; head[20] = 1; head[23] = 2; head[25] = 1; head[26] = 168;
  const contexts = new Uint8Array(80);
  const words = new DataView(contexts.buffer);
  words.setBigUint64(0, 0xfedcba9876543210n, true);
  words.setBigUint64(8, 0xabcdef0123456789n, true);
  const title = new TextEncoder().encode('Terminal');
  const agent = [1, 0, 0, 1, 0, 5, ...new TextEncoder().encode('codex')];
  const prefix = [...head, 7, 0, 0, 0, 0, title.length, 0, ...title, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0];
  const body = new Uint8Array([...prefix, 99, 3, 0, 1, 2, 3, 2, 80, 0, ...contexts, 1, agent.length, 0, ...agent]);
  const decoded = snapshot(body);
  assert.equal(decoded.agents.length, 1);
  assert.deepEqual(decoded.tabs[0].target, new Uint8Array([1, 0, ...contexts.subarray(0, 16), 7, 0, 0, 0]));
  const [model] = step(initialModel()[0], { kind: 'snapshot_loaded', body });
  assert.equal(model.railRows.length, 2);
  assert.deepEqual(model.railRows[0].target, decoded.tabs[0].target);
  assert.equal(model.railRows[1].agent, true);
  assert.equal(model.railRows[1].target.length, 0);
  assert.equal(snapshot(new Uint8Array([...prefix, 2, 79, 0, ...contexts.subarray(0, 79)])), null);
});
