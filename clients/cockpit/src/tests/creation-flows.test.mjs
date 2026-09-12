import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialModel, update } from '../core.ts';
import { localToolReply } from '../local-tools.ts';
import { newSessionReply } from '../new-session.ts';

const bytes = text => new TextEncoder().encode(text);
const text = value => new TextDecoder().decode(value);
const token = new Uint8Array([42, 0, 0, 0, 0, 0, 0, 0]);
const field = value => [value.length, 0, ...value];
const step = (model, msg) => { const result = update(model, msg); return Array.isArray(result) ? result : [result, null]; };
const toolReply = (phase, capture = token) => new Uint8Array([1, phase, 9, 0, 0, 0, ...capture, ...field(bytes('/tmp/config with spaces')), ...field(bytes(phase === 1 ? 'queued' : 'ready'))]);
const sessionReply = phase => new Uint8Array([1, phase, ...token, 9, 0, 0, 0, 4, ...bytes('mini'), 0]);

test('Add Machine retains destination through dedicated local setup and explicit recheck', () => {
  let [model, cmd] = step(initialModel()[0], { kind: 'add_machine_open' });
  assert.equal(model.hostOpen, true);
  assert.equal(cmd.cmds[1].name, 'cockpit.local-tools');
  assert.equal(cmd.cmds[1].payload[1], 1, 'describe captures an invoking window before setup');
  [model] = step(model, { kind: 'local_tool_loaded', body: toolReply(0) });
  [model] = step(model, { kind: 'host_edit', edit: { kind: 'insert_text', text: bytes('alice@mini') } });
  [model] = step(model, { kind: 'host_name_edit', edit: { kind: 'insert_text', text: bytes('Build machine') } });
  [model, cmd] = step(model, { kind: 'tool_submit' });
  assert.equal(cmd.cmds[1].payload[1], 3);
  assert.deepEqual(cmd.cmds[1].payload.slice(2, 10), token);
  assert.ok(text(cmd.cmds[1].payload).includes('alice@mini'));
  [model] = step(model, { kind: 'local_tool_loaded', body: toolReply(1) });
  assert.equal(model.hostOpen, false);
  assert.equal(model.toolQueued, true);
  assert.equal(text(model.hostQuery), 'alice@mini');
  [model, cmd] = step(model, { kind: 'tool_recheck' });
  assert.equal(model.navigatorView, 2);
  assert.equal(cmd.cmds[1].name, 'cockpit.machines');
  assert.equal(cmd.cmds[1].payload[1], 0, 'rechecking never authenticates');
});

test('Edit Configuration uses a captured local launch and never types into the focused shell', () => {
  let [model] = step(initialModel()[0], { kind: 'config_edit' });
  let cmd;
  [model, cmd] = step(model, { kind: 'local_tool_loaded', body: toolReply(0) });
  assert.equal(cmd.cmds[1].name, 'cockpit.local-tools');
  assert.equal(cmd.cmds[1].payload[1], 2);
  assert.deepEqual(cmd.cmds[1].payload.slice(2, 10), token);
  assert.equal(text(model.toolTarget), '/tmp/config with spaces');
  [model] = step(model, { kind: 'host_close' });
  assert.equal(step(model, { kind: 'local_tool_loaded', body: toolReply(1) })[0].hostOpen, false);
});

test('New Session names its captured machine, echoes token, and does not report pending as success', () => {
  let [model, cmd] = step(initialModel()[0], { kind: 'new_session_open' });
  assert.equal(cmd.cmds[1].name, 'cockpit.new-session');
  [model] = step(model, { kind: 'new_session_loaded', body: sessionReply(0) });
  assert.equal(text(model.renameTitle), 'New Session on mini');
  [model] = step(model, { kind: 'rename_edit', edit: { kind: 'insert_text', text: bytes('Build') } });
  [model, cmd] = step(model, { kind: 'rename_submit' });
  assert.deepEqual(cmd.cmds[1].payload.slice(2, 10), token);
  assert.equal(text(cmd.cmds[1].payload.slice(11)), 'Build');
  [model] = step(model, { kind: 'new_session_loaded', body: sessionReply(1) });
  assert.equal(model.renameOpen, true);
  assert.equal(model.newSessionAwaiting, true);
  [model] = step(model, { kind: 'new_session_loaded', body: sessionReply(2) });
  assert.equal(model.renameOpen, false);
});

test('local tools and new-session readers reject truncated captured replies', () => {
  const tool = toolReply(0);
  const session = sessionReply(0);
  for (let length = 0; length < tool.length; length++) assert.equal(localToolReply(tool.slice(0, length)), null);
  for (let length = 0; length < session.length; length++) assert.equal(newSessionReply(session.slice(0, length)), null);
});

function appearanceReply(active = true, dirty = false, outcome = 0) {
  return new Uint8Array([1, +active, outcome, +dirty, 0, 0, 0, 0, 0, 0]);
}

function settings(dirty = false) {
  const [opening] = step(initialModel()[0], { kind: 'settings_open' });
  return step(opening, { kind: 'appearance_loaded', body: appearanceReply(true, dirty) })[0];
}

test('each navigator destination waits for Settings rollback before opening', () => {
  for (const kind of ['commands_open', 'sessions_open', 'machines_open', 'windows_open', 'new_session_open', 'add_machine_open']) {
    let [model, cmd] = step(settings(true), { kind });
    assert.equal(model.settingsOpen, true, kind);
    assert.equal(model.paletteOpen, false, kind);
    assert.deepEqual([...cmd.payload], [1, 6, 0]);
    [model, cmd] = step(model, { kind: 'appearance_loaded', body: appearanceReply(false, false, 2) });
    assert.equal(model.settingsOpen, false, kind);
    assert.ok(model.paletteOpen || model.renameOpen || model.hostOpen, kind);
    assert.equal(cmd.cmds[0].name, 'cockpit.committed');
  }
});

test('failed Save and Edit cannot leave a delayed editor launch armed', () => {
  let [model] = step(settings(true), { kind: 'settings_edit_configuration' });
  assert.equal(model.configEditorConfirm, true);
  [model] = step(model, { kind: 'settings_save_edit' });
  assert.equal(model.pendingToolOpen, true);
  [model] = step(model, { kind: 'appearance_loaded', body: appearanceReply(true, true, 3) });
  assert.equal(model.pendingToolOpen, false);
  [model] = step(model, { kind: 'settings_close' });
  const [closed, cmd] = step(model, { kind: 'appearance_loaded', body: appearanceReply(false, false, 2) });
  assert.equal(closed.hostOpen, false);
  assert.equal(cmd.name, 'cockpit.committed');
});

test('Keyboard remapping refreshes actual dirty state and retains native rejection notice', () => {
  let [model] = step(settings(), { kind: 'settings_section', section: 2 });
  const command = bytes('commands.open'); const label = bytes('Commands'); const chord = bytes('super+p');
  const notice = bytes('Conflict with Go to Terminal');
  const body = new Uint8Array([1, 1, 1, notice.length, ...notice, 0, 0, command.length, label.length, chord.length, chord.length, ...command, ...label, ...chord, ...chord]);
  let cmd;
  [model, cmd] = step(model, { kind: 'keybindings_loaded', body });
  assert.equal(model.bindingRows.length, 1);
  assert.equal(text(model.settingsNotice), text(notice));
  assert.equal(cmd.name, 'cockpit.appearance');
  assert.deepEqual([...cmd.payload], [1, 0, 0], 'Begin is an idempotent transaction status refresh');
  [model] = step(model, { kind: 'appearance_loaded', body: appearanceReply(true, true) });
  assert.equal(model.appearanceBusy, false);
  assert.equal(model.appearance.dirty, true);
});

test('New Session cancellation suppresses late success and releases its input gate', () => {
  let [model] = step(initialModel()[0], { kind: 'new_session_open' });
  [model] = step(model, { kind: 'new_session_loaded', body: sessionReply(0) });
  let cmd;
  [model, cmd] = step(model, { kind: 'rename_close' });
  assert.equal(model.renameOpen, false);
  assert.equal(model.creatingSession, false);
  assert.equal(cmd.cmds.find(effect => effect.name === 'cockpit.new-session').payload[1], 4);
  const [late, effect] = step(model, { kind: 'new_session_loaded', body: sessionReply(2) });
  assert.deepEqual(late, model);
  assert.equal(effect, null);
});

test('New Session ignores another rename result and bounds names to the native wire limit', () => {
  let [model] = step(initialModel()[0], { kind: 'new_session_open' });
  [model] = step(model, { kind: 'new_session_loaded', body: sessionReply(0) });
  const [stale, effect] = step(model, { kind: 'session_loaded', body: new Uint8Array([1, 2, 0, 0, 0]) });
  assert.equal(stale.creatingSession, true);
  assert.equal(stale.renameOpen, true);
  assert.equal(effect, null);
  [model] = step(model, { kind: 'rename_edit', edit: { kind: 'insert_text', text: bytes('x'.repeat(241)) } });
  assert.ok(model.renameQuery.length <= 240);
});

test('keyboard Commands selection scrolls into the measured viewport and preserves wheel position', () => {
  let [model] = step(initialModel()[0], { kind: 'commands_open' });
  [model] = step(model, { kind: 'navigator_scrolled', scroll: { offsetX: 0, offsetY: 0, velocityX: 0, velocityY: 0,
    viewportExtentX: 500, viewportExtentY: 180, contentExtentX: 500, contentExtentY: 2000 } });
  for (let index = 0; index < 8; index++) [model] = step(model, { kind: 'palette_move', delta: 1 });
  assert.equal(model.paletteCursor, 8);
  assert.equal(model.navigatorScroll, 8 * 52 + 48 - 180);
  [model] = step(model, { kind: 'navigator_scrolled', scroll: { offsetX: 0, offsetY: 600, velocityX: 0, velocityY: 0,
    viewportExtentX: 500, viewportExtentY: 180, contentExtentX: 500, contentExtentY: 2000 } });
  assert.equal(model.navigatorScroll, 600);
  [model] = step(model, { kind: 'palette_move', delta: -1 });
  assert.equal(model.navigatorScroll, 7 * 52, 'moving back reveals the highlighted row after a wheel scroll');
});
