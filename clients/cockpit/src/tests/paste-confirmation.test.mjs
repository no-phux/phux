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
/// A minimal snapshot (no tabs, no themes, no secondary windows), the five
/// terminal-state bytes, then a `paste_confirmation` record (kind 7) when given.
function snapshotBytes(paste = null) {
  const base = new Uint8Array(38);
  base[0] = 1; base[1] = 2; base[10] = 7; base[23] = 2;
  base[26] = 168; base[29] = 255;
  if (paste === null) return base;
  const title = bytes(paste.title);
  const lines = [paste.lines & 255, (paste.lines >> 8) & 255, (paste.lines >> 16) & 255, (paste.lines >>> 24) & 255];
  const payload = [paste.window, paste.receiver, ...lines, title.length, ...title];
  return new Uint8Array([...base, 7, payload.length, 0, ...payload]);
}
function withPaste(paste) {
  return step(initialModel()[0], { kind: 'snapshot_loaded', body: snapshotBytes(paste) })[0];
}
function intents(cmd) {
  if (cmd === null) return [];
  const all = cmd.op === 'batch' ? cmd.cmds : [cmd];
  return all.filter(one => one.op === 'host_bytes' && one.name === 'cockpit.intent').map(one => one.payload);
}

test('the paste confirmation record decodes, and its absence holds nothing', () => {
  const decoded = snapshot(snapshotBytes({ window: 2, receiver: 1, lines: 300, title: 'vim' }));
  assert.equal(decoded.pasteConfirmation.window, 2);
  assert.equal(decoded.pasteConfirmation.receiver, 1);
  assert.equal(decoded.pasteConfirmation.lines, 300);
  assert.equal(text(decoded.pasteConfirmation.title), 'vim');
  assert.equal(snapshot(snapshotBytes()).pasteConfirmation.window, 255);
  // A torn or out-of-range record refuses the whole snapshot.
  const torn = snapshotBytes({ window: 0, receiver: 0, lines: 2, title: 'zsh' });
  assert.equal(snapshot(torn.subarray(0, torn.length - 1)), null);
  assert.equal(snapshot(snapshotBytes({ window: 5, receiver: 0, lines: 2, title: 'zsh' })), null);
  assert.equal(snapshot(snapshotBytes({ window: 0, receiver: 2, lines: 2, title: 'zsh' })), null);
});

test('the question shows in the window holding the terminal and names who receives the lines', () => {
  const shell = withPaste({ window: 0, receiver: 0, lines: 2, title: 'zsh' });
  assert.equal(shell.mainPasteOpen, true);
  assert.equal(shell.window1PasteOpen, false);
  assert.equal(text(shell.pasteTitle), 'Paste 2 lines into zsh?');
  assert.match(text(shell.pasteDetail), /shell is at its prompt and will run each line as a command/);
  const program = withPaste({ window: 1, receiver: 1, lines: 1, title: 'python3' });
  assert.equal(program.mainPasteOpen, false);
  assert.equal(program.window1PasteOpen, true);
  assert.equal(text(program.pasteTitle), 'Paste 1 line into python3?');
  assert.match(text(program.pasteDetail), /running program will receive each line as typed input/);
  // The next snapshot without the record takes the question down.
  const [cleared] = step(shell, { kind: 'snapshot_loaded', body: snapshotBytes() });
  assert.equal(cleared.mainPasteOpen, false);
  assert.equal(text(cleared.pasteTitle), '');
});

test('Paste and Cancel send the engine its confirm and cancel commands', () => {
  const model = withPaste({ window: 0, receiver: 0, lines: 2, title: 'zsh' });
  const [, confirm] = step(model, { kind: 'paste_confirm' });
  const [, cancel] = step(model, { kind: 'paste_cancel' });
  const [confirmIntent] = intents(confirm);
  const [cancelIntent] = intents(cancel);
  // version, native_command (11), revision, command, window 255 (focused).
  // The snapshot's engine revision (7) fences both.
  assert.deepEqual(Array.from(confirmIntent), [1, 11, 7, 0, 0, 0, 0, 0, 0, 0, 26, 255]);
  assert.deepEqual(Array.from(cancelIntent), [1, 11, 7, 0, 0, 0, 0, 0, 0, 0, 27, 255]);
  const markup = readFileSync(new URL('../windows/components/cockpit-window.native', import.meta.url), 'utf8');
  assert.match(markup, /<if test="\{pasteopen\}">/);
  assert.match(markup, /on-press="paste_confirm">Paste<\/button>/);
  assert.match(markup, /on-press="paste_cancel">Cancel<\/button>/);
  for (const [file, binding] of [['../app.native', 'mainPasteOpen'], ['../windows/phux-window-1.native', 'window1PasteOpen'],
    ['../windows/phux-window-4.native', 'window4PasteOpen']]) {
    assert.match(readFileSync(new URL(file, import.meta.url), 'utf8'), new RegExp(`pasteopen="\\{${binding}\\}"`));
  }
});
