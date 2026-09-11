import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { initialModel, update, commandMsg } from '../core.ts';
import { sessionRequest, sessionReply } from '../session.ts';

const bytes = value => new TextEncoder().encode(value);
const text = value => new TextDecoder().decode(value);
const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};
function reply(phase, name, host, reason = '') {
  const n = bytes(name);
  const h = bytes(host);
  const r = bytes(reason);
  return new Uint8Array([1, phase, n.length, ...n, h.length, ...h, r.length, ...r]);
}
function snapshotBytes(connection) {
  const out = new Uint8Array(33);
  out[0] = 1; out[1] = 2; out[10] = 7; out[23] = connection;
  out[26] = 168; out[29] = 255;
  return out;
}
function sessionRequests(cmd) {
  if (cmd === null) return [];
  const all = cmd.op === 'batch' ? cmd.cmds : [cmd];
  return all.filter(one => one.op === 'request' && one.name === 'cockpit.session').map(one => one.payload);
}
function opened(name = 'fixture', host = 'mini') {
  const [model] = step(initialModel()[0], { kind: 'rename_open' });
  return step(model, { kind: 'session_loaded', body: reply(0, name, host) })[0];
}

test('Rename Session is a Window menu command', () => {
  assert.deepEqual(commandMsg('session.rename'), { kind: 'rename_open' });
  const zon = readFileSync(new URL('../../app.zon', import.meta.url), 'utf8');
  assert.match(zon, /\.label = "Rename Session…", \.command = "session\.rename"/);
  const markup = readFileSync(new URL('../windows/components/cockpit-window.native', import.meta.url), 'utf8');
  assert.match(markup, /<template name="cockpit-rename" args="renameopen">/);
  for (const file of ['../app.native', '../windows/phux-window-1.native', '../windows/phux-window-4.native']) {
    assert.match(readFileSync(new URL(file, import.meta.url), 'utf8'), /<use template="cockpit-rename" renameopen="\{\w+RenameOpen\}" \/>/);
  }
});

test('opening asks which session is on screen, takes the modal slot, and seeds the field with its name', () => {
  let [model, cmd] = step(initialModel()[0], { kind: 'palette_open' });
  [model, cmd] = step(model, { kind: 'rename_open' });
  assert.equal(model.renameOpen, true);
  assert.equal(model.mainRenameOpen, true);
  assert.equal(model.paletteOpen, false);
  assert.equal(cmd.op, 'batch');
  assert.deepEqual(cmd.cmds[0], { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  assert.deepEqual(sessionRequests(cmd), [new Uint8Array([1, 1, 0])]);
  [model] = step(model, { kind: 'session_loaded', body: reply(0, 'fixture', 'mini') });
  assert.equal(text(model.renameQuery), 'fixture');
  assert.equal(text(model.renameTitle), 'Rename fixture on mini');
  assert.equal(model.renameBusy, false);
  // Another modal takes the slot back.
  [model] = step(model, { kind: 'palette_open' });
  assert.equal(model.renameOpen, false);
  assert.equal(model.mainRenameOpen, false);
});

test('nothing on screen to rename says so and sends nothing more', () => {
  let [model] = step(initialModel()[0], { kind: 'rename_open' });
  [model] = step(model, { kind: 'session_loaded', body: reply(4, '', '', 'No Phux session is on screen to rename.') });
  assert.equal(model.renameOpen, true);
  assert.equal(text(model.renameNotice), 'No Phux session is on screen to rename.');
});

test('submitting sends the new name; a refused rename keeps the panel and shows the reason', () => {
  let model = { ...opened(), renameQuery: bytes('deploy') };
  let cmd;
  [model, cmd] = step(model, { kind: 'rename_submit' });
  assert.deepEqual(sessionRequests(cmd), [sessionRequest(2, bytes('deploy'))]);
  assert.equal(model.renameBusy, true);
  // A second Enter while it is out sends nothing.
  assert.deepEqual(sessionRequests(step(model, { kind: 'rename_submit' })[1]), []);
  [model] = step(model, { kind: 'session_loaded', body: reply(3, 'fixture', 'mini', '"deploy" already exists on mini.') });
  assert.equal(model.renameOpen, true);
  assert.equal(model.renameBusy, false);
  assert.equal(text(model.renameNotice), '"deploy" already exists on mini.');
  assert.equal(text(model.renameQuery), 'deploy', 'the typed name stays for another try');
  // An empty name is refused here.
  [model, cmd] = step({ ...model, renameQuery: new Uint8Array() }, { kind: 'rename_submit' });
  assert.deepEqual(sessionRequests(cmd), []);
  assert.equal(text(model.renameNotice), 'Enter a new name for this session.');
});

test('a pending rename asks for its outcome on each snapshot, and a confirmed one closes the panel', () => {
  let model = { ...opened(), renameQuery: bytes('ship') };
  let cmd;
  [model] = step(model, { kind: 'rename_submit' });
  [model] = step(model, { kind: 'session_loaded', body: reply(1, 'fixture', 'mini') });
  assert.equal(model.renameAwaiting, true);
  [model, cmd] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  assert.deepEqual(sessionRequests(cmd), [new Uint8Array([1, 3, 0])]);
  [model, cmd] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  assert.deepEqual(sessionRequests(cmd), [new Uint8Array([1, 3, 0])], 'polled again while pending');
  [model, cmd] = step(model, { kind: 'session_loaded', body: reply(2, 'ship', 'mini') });
  assert.equal(model.renameOpen, false);
  assert.deepEqual(cmd, { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  // Settled: snapshots no longer ask.
  [model, cmd] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes(2) });
  assert.deepEqual(sessionRequests(cmd), []);
});

test('Escape closes the panel and gives the keyboard back', () => {
  let [model, cmd] = step(opened(), { kind: 'palette_close' });
  assert.equal(model.renameOpen, false);
  assert.deepEqual(cmd, { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  // A late answer after Escape changes nothing.
  [model] = step(model, { kind: 'session_loaded', body: reply(3, 'fixture', 'mini', 'late') });
  assert.equal(model.renameOpen, false);
});

test('the reply codec reads each field and refuses torn replies', () => {
  const decoded = sessionReply(reply(3, 'a', 'mini', 'why'));
  assert.equal(decoded.phase, 3);
  assert.equal(text(decoded.name), 'a');
  assert.equal(text(decoded.host), 'mini');
  assert.equal(text(decoded.reason), 'why');
  assert.equal(sessionReply(new Uint8Array([1, 2, 0, 0])), null);
  assert.equal(sessionReply(new Uint8Array([1, 9, 0, 0, 0])), null);
  assert.equal(sessionReply(new Uint8Array([2, 0, 0, 0, 0])), null);
  assert.deepEqual(sessionRequest(2, new Uint8Array(300)), new Uint8Array([1, 2, 0]));
});
