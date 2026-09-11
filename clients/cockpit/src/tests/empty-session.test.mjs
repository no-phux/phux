import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { initialModel, update } from '../core.ts';
import { snapshot } from '../protocol.ts';

const bytes = value => new TextEncoder().encode(value);
const text = value => new TextDecoder().decode(value);
const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};
/// A minimal snapshot (no tabs, no themes, no secondary windows), the
/// five terminal-state bytes, then an `empty_session` record when given.
function snapshotBytes(empty = null) {
  const base = new Uint8Array(38);
  base[0] = 1; base[1] = 2; base[10] = 7; base[23] = 2;
  base[26] = 168; base[29] = 255;
  if (empty === null) return base;
  const name = bytes(empty.name);
  const host = bytes(empty.host);
  const payload = [empty.windows, empty.flags, name.length, ...name, host.length, ...host];
  return new Uint8Array([...base, 4, payload.length, 0, ...payload]);
}
function sessionRequests(cmd) {
  if (cmd === null) return [];
  const all = cmd.op === 'batch' ? cmd.cmds : [cmd];
  return all.filter(one => one.op === 'request' && one.name === 'cockpit.session').map(one => one.payload);
}
function reply(phase, name, host, reason = '') {
  const n = bytes(name);
  const h = bytes(host);
  const r = bytes(reason);
  return new Uint8Array([1, phase, n.length, ...n, h.length, ...h, r.length, ...r]);
}
function withEmpty(empty) {
  return step(initialModel()[0], { kind: 'snapshot_loaded', body: snapshotBytes(empty) })[0];
}

test('the snapshot record decodes, and a snapshot without one has no empty session', () => {
  const decoded = snapshot(snapshotBytes({ windows: 5, flags: 1, name: 'scratch', host: 'mini' }));
  assert.equal(decoded.emptySession.windows, 5);
  assert.equal(decoded.emptySession.picked, true);
  assert.equal(decoded.emptySession.opening, false);
  assert.equal(text(decoded.emptySession.name), 'scratch');
  assert.equal(text(decoded.emptySession.host), 'mini');
  assert.equal(snapshot(snapshotBytes()).emptySession.windows, 0);
  // A torn record refuses the whole snapshot rather than guessing.
  const torn = snapshotBytes({ windows: 1, flags: 0, name: 'x', host: 'y' });
  assert.equal(snapshot(torn.subarray(0, torn.length - 1)), null);
  assert.equal(snapshot(snapshotBytes({ windows: 0, flags: 0, name: 'x', host: 'y' })), null);
});

test('an empty session renders its state, with New Tab, in the windows the snapshot names', () => {
  const model = withEmpty({ windows: 1, flags: 0, name: 'scratch', host: 'This Mac' });
  assert.equal(model.mainEmptyOpen, true);
  assert.equal(model.window1EmptyOpen, false);
  assert.equal(text(model.emptyName), 'scratch');
  assert.equal(text(model.emptyDetail), 'Empty session on This Mac');
  assert.equal(model.emptyPicked, false);
  const secondary = withEmpty({ windows: 2, flags: 1, name: 'scratch', host: 'mini' });
  assert.equal(secondary.mainEmptyOpen, false);
  assert.equal(secondary.window1EmptyOpen, true);
  assert.equal(secondary.emptyPicked, true);
  // No record: no state.
  const [cleared] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes() });
  assert.equal(cleared.mainEmptyOpen, false);
  assert.equal(cleared.emptyWindows, 0);
  const markup = readFileSync(new URL('../windows/components/cockpit-window.native', import.meta.url), 'utf8');
  assert.match(markup, /<template name="cockpit-empty" args="emptyopen">/);
  assert.match(markup, /on-press="empty_new_tab">New Tab<\/button>/);
  for (const file of ['../app.native', '../windows/phux-window-2.native']) {
    assert.match(readFileSync(new URL(file, import.meta.url), 'utf8'), /<use template="cockpit-empty" emptyopen="\{\w+EmptyOpen\}" \/>/);
  }
});

test('the state gives way to a modal and comes back when it closes', () => {
  let [model] = step(withEmpty({ windows: 1, flags: 0, name: 'scratch', host: 'This Mac' }), { kind: 'palette_open' });
  assert.equal(model.mainEmptyOpen, false);
  [model] = step(model, { kind: 'palette_close' });
  assert.equal(model.mainEmptyOpen, true);
});

test('New Tab asks the engine once; a refusal says why and New Tab works again', () => {
  let model = withEmpty({ windows: 1, flags: 1, name: 'scratch', host: 'mini' });
  let cmd;
  [model, cmd] = step(model, { kind: 'empty_new_tab' });
  assert.deepEqual(sessionRequests(cmd), [new Uint8Array([1, 4, 0])]);
  assert.equal(model.emptyBusy, true);
  assert.deepEqual(sessionRequests(step(model, { kind: 'empty_new_tab' })[1]), [], 'one at a time');
  [model] = step(model, { kind: 'empty_loaded', body: reply(3, '', '', 'That host is no longer connected.') });
  assert.equal(model.emptyBusy, false);
  assert.equal(text(model.emptyNotice), 'That host is no longer connected.');
  [model, cmd] = step(model, { kind: 'empty_new_tab' });
  assert.deepEqual(sessionRequests(cmd), [new Uint8Array([1, 4, 0])]);
  [model] = step(model, { kind: 'empty_loaded', body: reply(1, 'scratch', 'mini') });
  assert.equal(model.emptyBusy, true);
  assert.equal(text(model.emptyNotice), 'Opening a new tab in scratch...');
  // The tab landed: the next snapshot carries no record, and the state goes.
  [model] = step(model, { kind: 'snapshot_loaded', body: snapshotBytes() });
  assert.equal(model.mainEmptyOpen, false);
  assert.equal(model.emptyBusy, false);
});

test('only a picked empty session can be dismissed', () => {
  let [model, cmd] = step(withEmpty({ windows: 1, flags: 1, name: 'scratch', host: 'mini' }), { kind: 'empty_dismiss' });
  assert.deepEqual(sessionRequests(cmd), [new Uint8Array([1, 5, 0])]);
  [model, cmd] = step(withEmpty({ windows: 1, flags: 0, name: 'scratch', host: 'This Mac' }), { kind: 'empty_dismiss' });
  assert.deepEqual(sessionRequests(cmd), []);
});
