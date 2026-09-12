import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialMachines, requestMachines, machineRequest, machinePage, receiveMachines, moveMachine, capturedMachine, filterMachines } from '../machines.ts';

const bytes = text => new TextEncoder().encode(text);
const u32 = value => [value & 255, value >>> 8 & 255, value >>> 16 & 255, value >>> 24 & 255];
const field = text => { const value = bytes(text); return [value.length & 255, value.length >> 8, ...value]; };
function page(state, generation, names, first = 0, connection = 0) {
  const rows = names.flatMap((name, i) => [...u32(first + i), 1, 1, connection, ...field(name), ...field(`${name}@host:8788`), ...field(''), ...field('')]);
  return new Uint8Array([1, 0, ...u32(state.requestId), ...u32(generation), ...u32(names.length + first), ...u32(first), names.length, 0, ...field(''), ...rows]);
}

test('inventory supports more than four saved machines without connection claims', () => {
  const pending = requestMachines(initialMachines(), 0);
  const reply = page(pending, 12, ['a', 'b', 'c', 'd', 'e', 'f']);
  const state = receiveMachines(pending, reply, bytes(''));
  assert.equal(state.rows.length, 6);
  assert.ok(state.rows.every(row => !row.connected && new TextDecoder().decode(row.status) === 'Not connected'));
  assert.equal(machineRequest(pending, 0, bytes('')).length, 16);
  for (let i = 0; i < reply.length; i++) assert.equal(machinePage(reply.subarray(0, i)), null);
});

test('refresh preserves highlighted identity but invalidates captured authority', () => {
  let pending = requestMachines(initialMachines(), 0);
  let state = receiveMachines(pending, page(pending, 7, ['a', 'b']), bytes(''));
  state = moveMachine(moveMachine(state, 1, bytes('')), 1, bytes(''));
  const held = state.selected;
  pending = requestMachines(state, 0);
  state = receiveMachines(pending, page(pending, 8, ['b', 'a']), bytes(''));
  assert.deepEqual(state.selected, state.rows[0].target);
  assert.equal(capturedMachine(state, held), null);
  assert.equal(state.visible[0].highlighted, true);
  pending = requestMachines(state, 0);
  state = receiveMachines(pending, page(pending, 9, ['a']), bytes(''));
  assert.equal(state.selected.length, 0, 'removal clears the action instead of selecting the replacement row');
});

test('a late request cannot replace a newer machine list', () => {
  const old = requestMachines(initialMachines(), 0);
  const current = requestMachines(old, 0);
  assert.equal(receiveMachines(current, page(old, 2, ['old']), bytes('')), current);
});

test('single-row action receipts preserve the rest of the inventory', () => {
  let pending = requestMachines(initialMachines(), 0);
  let state = receiveMachines(pending, page(pending, 3, ['a', 'b', 'c']), bytes(''));
  pending = requestMachines(state, 2);
  state = receiveMachines(pending, page(pending, 3, ['b'], 1, 1), bytes(''));
  assert.equal(state.rows.length, 3);
  assert.equal(state.rows[1].state, 1);
  assert.equal(state.rows[0].state, 0);
  assert.equal(state.rows[2].state, 0);
});

test('filtering out the captured machine clears keyboard authority', () => {
  const pending = requestMachines(initialMachines(), 0);
  const listed = receiveMachines(pending, page(pending, 3, ['build', 'deploy']), bytes(''));
  const selected = moveMachine(listed, 1, bytes(''));
  assert.ok(selected.selected.length > 0);
  const filtered = filterMachines(selected, bytes('deploy'));
  assert.equal(filtered.visible.length, 1);
  assert.equal(filtered.selected.length, 0);
  assert.equal(capturedMachine(filtered, filtered.selected), null);
});
