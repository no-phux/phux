import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { initialModel, update, commandMsg } from '../core.ts';
import { directoryRequest, directoryPage, directoryRowLabel, DIR_HERE, DIR_UP } from '../directory.ts';

const bytes = value => new TextEncoder().encode(value);
const text = value => new TextDecoder().decode(value);
const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};
const u32 = n => [n % 256, Math.floor(n / 256) % 256, Math.floor(n / 65536) % 256, Math.floor(n / 16777216) % 256];
const u16 = n => [n % 256, Math.floor(n / 256)];

/// A `cockpit.directory` reply as the engine encodes it.
function reply({ status = 2, request = 1, truncated = false, total = 0, offset = 0, path = '/work', query = '', rows = [], message = '' } = {}) {
  const p = bytes(path);
  const q = bytes(query);
  const m = bytes(message);
  const out = [1, status, ...u32(request), truncated ? 1 : 0, 0, ...u16(total), ...u16(offset), p.length, ...p, q.length, ...q, rows.length];
  for (const [index, name, symlink] of rows) {
    const n = bytes(name);
    out.push(...u16(index), symlink ? 1 : 0, n.length, ...n);
  }
  out.push(m.length, ...m);
  return new Uint8Array(out);
}
function request(kind, id, offset, index, query = '') {
  return new Uint8Array([1, kind, ...u32(id), ...u16(offset), ...u16(index), bytes(query).length, ...bytes(query)]);
}
function directoryCommand(cmd) {
  if (cmd === null) return undefined;
  if (cmd.op === 'batch') return cmd.cmds.find(each => each.name === 'cockpit.directory');
  return cmd.name === 'cockpit.directory' ? cmd : undefined;
}
function assertDirectoryRequest(cmd, payload) {
  const found = directoryCommand(cmd);
  assert.ok(found, 'a cockpit.directory request');
  assert.equal(found.op, 'request');
  assert.equal(found.name, 'cockpit.directory');
  assert.deepEqual(found.payload, payload);
}
function snapshotBytes(activeWindow = 0) {
  const out = new Uint8Array(33);
  out[0] = 1; out[1] = 2; out[10] = 7; out[18] = activeWindow; out[23] = 2;
  out[26] = 168; out[29] = 255;
  return out;
}
const invalidation = () => {
  const event = new Uint8Array(18);
  event[0] = 1; event[1] = 1; event[2] = 3;
  return event;
};
const LISTED = reply({ total: 5, rows: [[DIR_HERE, ''], [DIR_UP, ''], [0, '.config'], [1, 'cockpit']] });

function opened() {
  return step(initialModel()[0], { kind: 'dir_open' });
}
/// Open, see the engine start listing (request 1), then the settled page.
function listed() {
  let [model] = opened();
  [model] = step(model, { kind: 'directory_loaded', body: reply({ status: 1, path: '' }) });
  [model] = step(model, { kind: 'directory_loaded', body: LISTED });
  return model;
}

test('Go to Directory is a menu command with its own chord, not cmd+shift+G', () => {
  assert.deepEqual(commandMsg('directory.open'), { kind: 'dir_open' });
  const zon = readFileSync(new URL('../../app.zon', import.meta.url), 'utf8');
  assert.match(zon, /\.label = "Go to Directory…", \.command = "directory\.open", \.key = "j", \.modifiers = \.\{ "primary", "shift" \}/);
  assert.match(zon, /\.id = "directory\.open", \.key = "j", \.modifiers = \.\{ "primary", "shift" \}/);
  const shiftJ = zon.split('\n').filter(line => /\.key = "j", \.modifiers = \.\{ "primary", "shift" \}/.test(line));
  for (const line of shiftJ) assert.match(line, /directory\.open/);
  const shiftG = zon.split('\n').filter(line => /\.key = "g", \.modifiers = \.\{ "primary", "shift" \}/.test(line));
  assert.ok(shiftG.length > 0);
  for (const line of shiftG) assert.match(line, /terminal\.find-previous/);
});

test('opening takes the modal slot, asks the engine to start, and is scoped to the invoking window', () => {
  let [model, cmd] = step(initialModel()[0], { kind: 'palette_open' });
  [model, cmd] = step(model, { kind: 'dir_open' });
  assert.equal(model.dirOpen, true);
  assert.equal(model.paletteOpen, false);
  assert.equal(model.mainDirOpen, true);
  assert.equal(cmd.op, 'batch');
  assert.deepEqual(cmd.cmds[0], { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  assertDirectoryRequest(cmd, request(1, 0, 0, 0));
  // Connect to Host takes the slot back.
  [model] = step(model, { kind: 'host_open' });
  assert.equal(model.dirOpen, false);
  assert.equal(model.hostOpen, true);

  [model] = step(initialModel()[0], { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  [model] = step(model, { kind: 'dir_open' });
  assert.equal(model.mainDirOpen, false);
  assert.equal(model.window2DirOpen, true);
});

test('a pending listing is polled on each invalidation until it settles into labelled rows', () => {
  let [model] = opened();
  let cmd;
  [model, cmd] = step(model, { kind: 'directory_loaded', body: reply({ status: 1, path: '' }) });
  assert.equal(cmd, null);
  assert.equal(model.dirAwaiting, true);
  assert.equal(text(model.dirNotice), 'Listing home...');
  [model, cmd] = step(model, { kind: 'engine_event', key: 0, state: 'data', bytes: invalidation(), droppedPending: 0, droppedTotal: 0 });
  assert.equal(cmd.op, 'batch');
  assertDirectoryRequest(cmd, request(2, 1, 0, 0));
  [model] = step(model, { kind: 'directory_loaded', body: LISTED });
  assert.equal(model.dirAwaiting, false);
  assert.deepEqual(model.dirRows.map(row => text(row.label)), ['Open a new tab here', '..', '.config/', 'cockpit/']);
  assert.equal(model.dirRows[0].highlighted, true);
  assert.equal(model.dirNext, true);
  assert.equal(text(model.dirPath), '/work');
  // Settled: an invalidation no longer asks.
  [, cmd] = step(model, { kind: 'engine_event', key: 0, state: 'data', bytes: invalidation(), droppedPending: 0, droppedTotal: 0 });
  assert.equal(directoryCommand(cmd), undefined);
});

test('Enter on a directory descends with the listing it came from; the next reply names the new one', () => {
  let model = listed();
  let cmd;
  for (let i = 0; i < 3; i += 1) [model] = step(model, { kind: 'palette_move', delta: 1 });
  assert.equal(model.dirRows[3].highlighted, true);
  [model, cmd] = step(model, { kind: 'dir_submit' });
  assertDirectoryRequest(cmd, request(3, 1, 0, 1));
  assert.equal(model.dirStarting, true);
  assert.equal(model.dirRows.length, 0);
  [model] = step(model, { kind: 'directory_loaded', body: reply({ status: 1, request: 2, path: '/work/cockpit' }) });
  assert.deepEqual([...model.dirRequest], u32(2));
  // `..` goes up from there.
  [model] = step(model, { kind: 'directory_loaded', body: reply({ request: 2, path: '/work/cockpit', total: 2, rows: [[DIR_HERE, ''], [DIR_UP, '']] }) });
  [, cmd] = step(model, { kind: 'dir_pick', index: DIR_UP });
  assertDirectoryRequest(cmd, request(4, 2, 0, 0));
});

test('Escape cancels, and a reply that arrives afterwards is dropped', () => {
  let model = listed();
  let cmd;
  [model, cmd] = step(model, { kind: 'palette_close' });
  assert.equal(model.dirOpen, false);
  assert.deepEqual(cmd, { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  const closed = model;
  [model, cmd] = step(model, { kind: 'directory_loaded', body: reply({ request: 2, path: '/late', total: 1, rows: [[0, 'late']] }) });
  assert.equal(cmd, null);
  assert.equal(model, closed);
  assert.equal(model.dirOpen, false);
});

test('a reply for a listing the picker has left, or for an older filter, changes nothing', () => {
  const model = listed();
  const [stale] = step(model, { kind: 'directory_loaded', body: reply({ request: 9, total: 1, rows: [[0, 'other']] }) });
  assert.equal(stale, model);
  let [filtered, cmd] = step(model, { kind: 'dir_edit', edit: { kind: 'insert_text', text: bytes('co') } });
  assertDirectoryRequest(cmd, request(2, 1, 0, 0, 'co'));
  const [older] = step(filtered, { kind: 'directory_loaded', body: LISTED });
  assert.equal(older.dirRows.length, filtered.dirRows.length, 'a page for the empty filter does not replace the typed one');
  [filtered] = step(filtered, { kind: 'directory_loaded', body: reply({ query: 'co', total: 1, rows: [[1, 'cockpit']] }) });
  assert.deepEqual(filtered.dirRows.map(row => text(row.label)), ['cockpit/']);
});

test('Open Here asks the engine for a new tab and closes once it is accepted', () => {
  let model = listed();
  let cmd;
  [model, cmd] = step(model, { kind: 'dir_here' });
  assertDirectoryRequest(cmd, request(5, 1, 0, DIR_HERE));
  assert.equal(model.dirClosing, true);
  [model, cmd] = step(model, { kind: 'directory_loaded', body: LISTED });
  assert.equal(model.dirOpen, false);
  assert.deepEqual(cmd, { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });

  // A refusal keeps the picker for another try.
  model = listed();
  [model] = step(model, { kind: 'dir_pick', index: DIR_HERE });
  [model] = step(model, { kind: 'directory_failed', error: bytes('Refused') });
  assert.equal(model.dirOpen, true);
  assert.equal(model.dirClosing, false);
  assert.match(text(model.dirNotice), /Could not open a new tab/);
});

test('refusals and unsupported servers are named in the notice', () => {
  let [model] = opened();
  [model] = step(model, { kind: 'directory_loaded', body: reply({ status: 0, request: 0, path: '', message: 'This coordinator cannot list directories. Update phux on that host.' }) });
  assert.match(text(model.dirNotice), /cannot list directories/);
  [model] = opened();
  [model] = step(model, { kind: 'directory_loaded', body: reply({ status: 3, path: '/root', total: 1, rows: [[DIR_UP, '']], message: 'permission denied' }) });
  assert.equal(text(model.dirNotice), 'Could not list /root: permission denied');
  assert.deepEqual(model.dirRows.map(row => text(row.label)), ['..']);
});

test('a connection change withdraws the rows, and the picker lists again once reconnected', () => {
  const connection = value => {
    const out = snapshotBytes(0);
    out[23] = value;
    return out;
  };
  let [model] = step(initialModel()[0], { kind: 'snapshot_loaded', body: connection(2) });
  [model] = step(model, { kind: 'dir_open' });
  [model] = step(model, { kind: 'directory_loaded', body: reply({ status: 1, path: '' }) });
  [model] = step(model, { kind: 'directory_loaded', body: LISTED });
  assert.equal(model.dirRows.length, 4);
  let cmd;
  [model, cmd] = step(model, { kind: 'snapshot_loaded', body: connection(1) });
  assert.equal(model.dirOpen, true);
  assert.equal(model.dirRows.length, 0, 'no rows from the old connection');
  assert.equal(text(model.dirNotice), 'Waiting for the connection...');
  assert.equal(directoryCommand(cmd), undefined);
  [model, cmd] = step(model, { kind: 'snapshot_loaded', body: connection(2) });
  assertDirectoryRequest(cmd, request(1, 0, 0, 0));
  assert.equal(model.dirStarting, true);
  [model] = step(model, { kind: 'directory_loaded', body: reply({ status: 1, request: 5, path: '' }) });
  assert.deepEqual([...model.dirRequest], u32(5));
});

test('the codec frames requests exactly and refuses what it cannot read', () => {
  assert.deepEqual(directoryRequest(2, new Uint8Array([7, 0, 0, 0]), 260, 3, bytes('ab')), new Uint8Array([1, 2, 7, 0, 0, 0, 4, 1, 3, 0, 2, 97, 98]));
  assert.equal(directoryRequest(2, new Uint8Array(4), 0, 0, new Uint8Array(65))[10], 0, 'an overlong query is sent empty');
  const parsed = directoryPage(reply({ truncated: true, total: 1, rows: [[2, 'phux', true]] }));
  assert.equal(parsed.truncated, true);
  assert.equal(text(directoryRowLabel(parsed.rows[0])), 'phux/  (link)');
  assert.equal(directoryPage(new Uint8Array([2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])), null, 'version');
  const good = reply({ total: 1, rows: [[0, 'a']] });
  assert.equal(directoryPage(good.subarray(0, good.length - 1)), null, 'truncated reply');
  assert.equal(directoryPage(new Uint8Array([...good, 0])), null, 'trailing bytes');
});

test('every window presents the picker through its own flag', () => {
  const read = path => readFileSync(new URL(path, import.meta.url), 'utf8');
  assert.match(read('../windows/components/cockpit-window.native'), /<template name="cockpit-directory" args="diropen">/);
  const use = flag => new RegExp(`<use template="cockpit-directory" diropen="\\{${flag}\\}" />`);
  assert.match(read('../app.native'), use('mainDirOpen'));
  for (const n of [1, 2, 3, 4]) assert.match(read(`../windows/phux-window-${n}.native`), use(`window${n}DirOpen`));
});
