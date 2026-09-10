import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialCommandResults, requestCommandResults, receiveCommandResult, failedCommandResults } from '../command-results.ts';

function packet(id = 1n, operation = 1, placement = 1, focus = 1) {
  const bytes = new Uint8Array(71);
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
  view.setUint16(66, 3, true);
  bytes.set([7, 8, 9], 68);
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
  assert.deepEqual(result.mutationTicket, { hi: 0xabcdef01, lo: 0x23456789 });
  assert.equal(new DataView(received.request.buffer).getBigUint64(2, true), 0xfedcba9876543210n);
  bytes[68] = 99;
  assert.deepEqual(result.terminal, new Uint8Array([7, 8, 9]));
  const end = receiveCommandResult(received.state, new Uint8Array([1, 0]));
  assert.equal(end.state.loading, false);
  assert.equal(end.request.length, 0);
});

test('result delivery failure cannot erase a receipt or retry the original operation', () => {
  const first = receiveCommandResult(initialCommandResults(), packet());
  const invalidPackets = [new Uint8Array(), packet().slice(0, 70), packet(0n), new Uint8Array([1, 0, 0])];
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
  const bytes = new Uint8Array(68 + 274);
  bytes.set(packet().subarray(0, 68));
  new DataView(bytes.buffer).setUint16(66, 274, true);
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
    const bytes = new Uint8Array(68 + length);
    bytes.set(packet().subarray(0, 68));
    new DataView(bytes.buffer).setUint16(66, length, true);
    assert.equal(receiveCommandResult(initial.state, bytes).request.length, 10);
  }
  for (const [offset, value] of [[0, 2], [1, 0], [1, 4], [2, 0], [2, 4], [3, 6], [4, 4]]) {
    const bytes = packet();
    bytes[offset] = value;
    const rejected = receiveCommandResult(initial.state, bytes);
    assert.equal(rejected.request.length, 0);
    assert.equal(rejected.state.acknowledgement, initial.request);
  }
  assert.equal(receiveCommandResult(initial.state, new Uint8Array([...packet(), 0])).request.length, 0);
});
