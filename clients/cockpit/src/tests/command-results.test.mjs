import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialCommandResults, requestCommandResults, receiveCommandResult, failedCommandResults } from '../command-results.ts';
import { initialModel, update } from '../core.ts';

const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};

function packet(id = 1n, operation = 1, placement = 1, focus = 1) {
  const bytes = new Uint8Array(77);
  bytes.set([1, 1, operation, placement, focus, 0]);
  const view = new DataView(bytes.buffer);
  view.setBigUint64(6, id, true);
  view.setBigUint64(14, 0xfedcba9876543210n, true);
  view.setUint32(22, 0xffffffff, true);
  view.setBigUint64(26, 0xabcdef0123456789n, true);
  view.setUint32(34, 0xfffffffe, true);
  view.setUint32(38, 0xfffffffd, true);
  view.setBigUint64(50, 0xfedcba9876543211n, true);
  view.setBigUint64(58, 0xfedcba9876543212n, true);
  bytes[66] = 1;
  view.setUint32(68, 0xfedcba98, true);
  view.setUint16(72, 3, true);
  bytes.set([7, 8, 9], 74);
  return bytes;
}

test('eventual results retain full correlation and acknowledge only after decoding', () => {
  const start = requestCommandResults(initialCommandResults());
  assert.deepEqual(start.request, new Uint8Array([1, 0]));
  assert.equal(requestCommandResults(start.state).request.length, 0);
  const bytes = packet(0xfedcba9876543210n);
  const received = receiveCommandResult(start.state, bytes);
  const result = received.state.recent[0];
  assert.deepEqual(result.id, { hi: 0xfedcba98, lo: 0x76543210 });
  assert.deepEqual(result.operationEpoch, result.id);
  assert.equal(result.operationRequest, 0xffffffff);
  assert.equal(result.attachmentRequest, 0xfffffffe);
  assert.equal(result.placementRequest, 0xfffffffd);
  assert.deepEqual(result.attachmentEpoch, { hi: 0xfedcba98, lo: 0x76543211 });
  assert.deepEqual(result.placementEpoch, { hi: 0xfedcba98, lo: 0x76543212 });
  assert.equal(result.mutationOutcome, 1);
  assert.equal(result.targetSessionId, 0xfedcba98);
  assert.deepEqual(result.mutationTicket, { hi: 0xabcdef01, lo: 0x23456789 });
  assert.equal(new DataView(received.request.buffer).getBigUint64(2, true), 0xfedcba9876543210n);
  bytes[74] = 99;
  assert.deepEqual(result.terminal, new Uint8Array([7, 8, 9]));
  const end = receiveCommandResult(received.state, new Uint8Array([1, 0]));
  assert.equal(end.state.loading, false);
  assert.equal(end.request.length, 0);
});

test('result delivery failure cannot erase a receipt or retry the original operation', () => {
  const first = receiveCommandResult(initialCommandResults(), packet());
  const invalidPackets = [new Uint8Array(), packet().slice(0, 76), packet(0n), new Uint8Array([1, 0, 0])];
  for (const bytes of invalidPackets) {
    const failure = receiveCommandResult(first.state, bytes);
    assert.equal(failure.request.length, 0);
    assert.equal(failure.state.recent, first.state.recent);
    assert.equal(failure.state.acknowledgement, first.request);
    assert.deepEqual(requestCommandResults(failure.state).request, first.request);
  }
  const failure = failedCommandResults(first.state);
  assert.match(new TextDecoder().decode(failure.state.deliveryNotice), /may still be running/);
  const duplicate = receiveCommandResult(failure.state, packet());
  assert.equal(duplicate.state.recent.length, 1);
  assert.equal(duplicate.state.deliveryNotice.length, 0);
});

test('operation success survives refused placement and superseded focus', () => {
  let decision = receiveCommandResult(initialCommandResults(), packet(1n, 1, 3, 2));
  assert.equal(decision.state.recent[0].operation, 1);
  assert.equal(decision.state.recent[0].focus, 2);
  assert.match(new TextDecoder().decode(decision.state.notice), /succeeded.*destination/);
  decision = receiveCommandResult(decision.state, packet(2n, 1, 1, 2));
  assert.equal(decision.state.recent[1].placement, 1);
  assert.match(new TextDecoder().decode(decision.state.notice), /succeeded.*destination/);
  const quiet = receiveCommandResult(initialCommandResults(), packet(2n, 1, 1, 2));
  assert.equal(quiet.state.notice.length, 0);
  const unknown = receiveCommandResult(initialCommandResults(), packet(3n, 3, 4, 3));
  assert.match(new TextDecoder().decode(unknown.state.notice), /unknown/);
  assert.equal(unknown.state.recent[0].operation, 3);
});

test('consumed result history is bounded while acknowledgements advance exactly', () => {
  let state = initialCommandResults();
  for (let id = 1n; id <= 40n; id++) state = receiveCommandResult(state, packet(id)).state;
  assert.equal(state.recent.length, 16);
  assert.deepEqual(state.recent[0].id, { hi: 0, lo: 25 });
  assert.equal(new DataView(state.acknowledgement.buffer).getBigUint64(2, true), 40n);
});

test('an invalidation during an empty read is not lost', () => {
  let decision = requestCommandResults(initialCommandResults());
  decision = requestCommandResults(decision.state);
  assert.equal(decision.request.length, 0);
  decision = receiveCommandResult(decision.state, new Uint8Array([1, 0]));
  assert.deepEqual(decision.request, new Uint8Array([1, 0]));
  assert.equal(decision.state.loading, true);
  decision = receiveCommandResult(decision.state, packet(9n));
  assert.deepEqual(decision.state.recent[0].id, { hi: 0, lo: 9 });
});

test('delivery errors and duplicates cannot erase or roll back the retained exception', () => {
  let decision = receiveCommandResult(initialCommandResults(), packet(1n, 2, 2, 3));
  decision = receiveCommandResult(decision.state, packet(2n, 3, 4, 3));
  const notice = decision.state.notice;
  decision = failedCommandResults(decision.state);
  assert.deepEqual(decision.state.notice, notice);
  decision = receiveCommandResult(decision.state, packet(1n, 2, 2, 3));
  assert.deepEqual(decision.state.notice, notice);
  assert.equal(new DataView(decision.request.buffer).getBigUint64(2, true), 1n);
  for (let id = 3n; id <= 30n; id++) decision = receiveCommandResult(decision.state, packet(id));
  assert.deepEqual(decision.state.notice, notice);
  assert.equal(decision.state.recent.length, 16);
});

test('oversized terminal bytes are never acknowledged', () => {
  const initial = receiveCommandResult(initialCommandResults(), packet());
  const bytes = new Uint8Array(74 + 274);
  bytes.set(packet().subarray(0, 74));
  new DataView(bytes.buffer).setUint16(72, 274, true);
  const rejected = receiveCommandResult(initial.state, bytes);
  assert.equal(rejected.request.length, 0);
  assert.equal(rejected.state.acknowledgement, initial.request);
});

test('equal command IDs from independent origins never alias and retain typed reasons', () => {
  let state = initialCommandResults();
  for (const source of [1, 2, 3]) {
    const bytes = packet(0xfedcba9876543210n);
    bytes[1] = source;
    bytes[5] = 7;
    new DataView(bytes.buffer).setUint32(42, 0xabcdef01, true);
    new DataView(bytes.buffer).setUint32(46, 0xfedcba98, true);
    const decision = receiveCommandResult(state, bytes);
    assert.equal(decision.request[1], source);
    assert.equal(new DataView(decision.request.buffer).getBigUint64(2, true), 0xfedcba9876543210n);
    state = decision.state;
  }
  assert.equal(state.recent.length, 3);
  assert.equal(state.recent[2].reason, 7);
  assert.equal(state.recent[2].errorDomain, 0xabcdef01);
  assert.equal(state.recent[2].errorCode, 0xfedcba98);
});

test('wire boundaries reject malformed records without advancing acknowledgement', () => {
  const initial = receiveCommandResult(initialCommandResults(), packet());
  for (const length of [0, 273]) {
    const bytes = new Uint8Array(74 + length);
    bytes.set(packet().subarray(0, 74));
    new DataView(bytes.buffer).setUint16(72, length, true);
    assert.equal(receiveCommandResult(initial.state, bytes).request.length, 10);
  }
  for (const [offset, value] of [[0, 2], [1, 0], [1, 4], [2, 0], [2, 4], [3, 6], [4, 4], [66, 4], [67, 1]]) {
    const bytes = packet();
    bytes[offset] = value;
    const rejected = receiveCommandResult(initial.state, bytes);
    assert.equal(rejected.request.length, 0);
    assert.equal(rejected.state.acknowledgement, initial.request);
  }
  assert.equal(receiveCommandResult(initial.state, new Uint8Array([...packet(), 0])).request.length, 0);
});

test('creation admission and eventual outcomes have independent correlation', () => {
  let [model, command] = step(initialModel()[0], { kind: 'new_terminal' });
  assert.equal(command.name, 'cockpit.tab-command');
  assert.equal(command.payload[1], 3);
  const first = command.payload;
  [model, command] = step(model, { kind: 'native_command', command: 4 });
  assert.equal(command, null);
  assert.equal(model.tabCommands.queue.length, 2);
  [model, command] = step(model, { kind: 'command_result_loaded', body: packet(99n, 3, 4, 3) });
  assert.equal(command.name, 'cockpit.command-results');
  assert.equal(model.tabCommands.queue.length, 2);
  const receipt = new Uint8Array(27);
  receipt.set([1, 3, 0]);
  receipt.set(first.subarray(2, 10), 3);
  [model, command] = step(model, { kind: 'tab_command_completed', body: receipt });
  assert.equal(model.tabCommands.outcome, 7);
  assert.equal(command.name, 'cockpit.tab-command');
  assert.equal(command.payload[11], 11);
  assert.equal(command.payload[20], 4);
  assert.equal(model.commandResults.recent[0].operation, 3);
  assert.match(new TextDecoder().decode(model.commandNotice), /unknown/);
});

test('the matching eventual result clears pending admission without inventing focus failure', () => {
  let [model, command] = step(initialModel()[0], { kind: 'new_terminal' });
  const receipt = new Uint8Array(27);
  receipt.set([1, 3, 0]);
  receipt.set(command.payload.subarray(2, 10), 3);
  [model] = step(model, { kind: 'tab_command_completed', body: receipt });
  assert.match(new TextDecoder().decode(model.commandNotice), /accepted.*pending/);
  [model, command] = step(model, { kind: 'command_result_loaded', body: packet(1n, 1, 1, 2) });
  assert.equal(model.tabCommands.outcome, 7); // The original admission is still true.
  assert.equal(model.commandResults.recent[0].focus, 2);
  assert.equal(model.commandNotice.length, 0);
  assert.equal(command.name, 'cockpit.command-results');
  assert.equal(new DataView(command.payload.buffer).getBigUint64(2, true), 1n);
});

test('failed read consumes one buffered completion wake without an unconditional retry loop', () => {
  let decision = requestCommandResults(initialCommandResults());
  decision = requestCommandResults(decision.state);
  decision = failedCommandResults(decision.state);
  assert.deepEqual(decision.request, new Uint8Array([1, 0]));
  assert.equal(decision.state.loading, true);
  assert.equal(decision.state.refreshPending, false);
  decision = failedCommandResults(decision.state);
  assert.equal(decision.request.length, 0);
  assert.equal(decision.state.loading, false);
});

test('successful empty recovery clears the displayed transient delivery warning', () => {
  let [model] = step(initialModel()[0], { kind: 'command_result_failed', error: new Uint8Array() });
  assert.match(new TextDecoder().decode(model.commandNotice), /result unavailable/);
  [model] = step(model, { kind: 'command_result_loaded', body: new Uint8Array([1, 0]) });
  assert.equal(model.commandResults.deliveryNotice.length, 0);
  assert.equal(model.commandNotice.length, 0);
});
