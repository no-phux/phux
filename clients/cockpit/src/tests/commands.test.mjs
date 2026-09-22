import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { initialModel, update, commandMsg } from '../core.ts';
import { COMMAND_CATALOG } from '../command-catalog.ts';
import { generatedCatalog, generatedShippingFixture } from '../../scripts/command-catalog.mjs';

const bytes = text => new TextEncoder().encode(text);
const text = bytes => new TextDecoder().decode(bytes);
const step = (model, message) => {
  const result = update(model, message);
  return Array.isArray(result) ? result : [result, null];
};

test('command catalog derives exactly from shipping menu labels and shortcuts', () => {
  const manifest = readFileSync(new URL('../../app.zon', import.meta.url), 'utf8');
  const generated = readFileSync(new URL('../command-catalog.ts', import.meta.url), 'utf8');
  assert.equal(generated, generatedCatalog(manifest));
  const fixture = readFileSync(new URL('./shipping_commands.zig', import.meta.url), 'utf8');
  assert.equal(fixture, generatedShippingFixture(manifest));
  assert.match(fixture, /\.id = "commands.open", \.key = "p", \.modifiers = \.\{ \.primary = true, \.shift = true \}/);
  assert.equal((fixture.match(/\.command = /g) ?? []).length, COMMAND_CATALOG.length);
  assert.equal(text(COMMAND_CATALOG.find(command => command.name === 'commands.open').shortcut), 'Cmd+Shift+P');
  assert.equal(text(COMMAND_CATALOG.find(command => command.name === 'terminal.clear').shortcut), 'Cmd+K');
  assert.equal(text(COMMAND_CATALOG.find(command => command.name === 'terminal.find-previous').shortcut), 'Cmd+Shift+G');
  assert.equal(text(COMMAND_CATALOG.find(command => command.name === 'tabs.palette').shortcut), '');
});

test('CmdShiftP opens actions; action selection uses the same message as native menus', () => {
  let [model] = step(initialModel()[0], commandMsg('commands.open'));
  assert.equal(model.paletteOpen, true);
  assert.equal(model.navigatorView, 4);
  [model] = step(model, { kind: 'palette_edit', edit: { kind: 'insert_text', text: bytes('New Tab') } });
  assert.equal(model.actionRows.length, 1);
  assert.equal(model.actionRows[0].disabled, false);
  const [after, action] = step(model, { kind: 'palette_submit' });
  const [, native] = step(initialModel()[0], commandMsg('terminal.new'));
  assert.equal(after.paletteOpen, false);
  assert.equal(action.cmds[0].name, 'cockpit.committed');
  assert.deepEqual(action.cmds[1], native);
});

test('disabled actions explain missing terminal and Enter cannot execute them', () => {
  let [model] = step(initialModel()[0], commandMsg('commands.open'));
  [model] = step(model, { kind: 'palette_edit', edit: { kind: 'insert_text', text: bytes('Split Right') } });
  assert.equal(model.actionRows.length, 1);
  assert.equal(model.actionRows[0].disabled, true);
  assert.equal(text(model.actionRows[0].detail), 'Requires a focused terminal');
  assert.equal(step(model, { kind: 'palette_submit' })[1], null);
  [model] = step(model, { kind: 'palette_close' });
  assert.equal(model.paletteOpen, false);
});

test('navigator switches views directly and Machines requests real inventory', () => {
  let [model] = step(initialModel()[0], { kind: 'commands_open' });
  let request;
  [model, request] = step(model, { kind: 'machines_open' });
  assert.equal(model.paletteOpen, true);
  assert.equal(model.navigatorView, 2);
  assert.equal(request.cmds[1].name, 'cockpit.machines');
  assert.equal(model.machines.rows.length, 0, 'the frontend never invents a This Mac or saved-host row');
  [model, request] = step(model, { kind: 'sessions_open' });
  assert.equal(model.navigatorView, 1);
  assert.equal(request.cmds[1].name, 'cockpit.navigation');
});

test('Commands captures an off-strip secondary tab and refuses retargeting', () => {
  const held = new Uint8Array(22); held[0] = 1; held[21] = 7;
  const replacement = held.slice(); replacement[21] = 8;
  const row = { id: 1, index: 8, label: bytes('Selected off strip'), state: bytes(''), mark: bytes(''), selected: true, agent: false, target: held };
  const incoming = { ...initialModel()[0], activeWindow: 1, window1Open: true, window1RailRows: [row] };
  let [model] = step(incoming, { kind: 'commands_open' });
  assert.deepEqual(model.commandContextTarget, held);
  [model] = step(model, { kind: 'palette_edit', edit: { kind: 'insert_text', text: bytes('Minimize') } });
  const changed = { ...model, window1RailRows: [{ ...row, target: replacement }] };
  const [refused, effect] = step(changed, { kind: 'palette_submit' });
  assert.equal(effect, null);
  assert.equal(refused.paletteOpen, true);
  assert.match(text(refused.paletteNotice), /captured context/);
});

test('All work returns from every navigator view and clears the previous scope', () => {
  for (const kind of ['sessions_open', 'machines_open', 'windows_open', 'commands_open']) {
    const [opened] = step(initialModel()[0], { kind });
    assert.notEqual(opened.navigatorView, 0);
    const [model, request] = step(opened, { kind: 'palette_open' });
    assert.equal(model.paletteOpen, true);
    assert.equal(model.navigatorView, 0, kind);
    assert.equal(model.paletteScope, 0, kind);
    assert.equal(text(model.paletteQuery), '');
    assert.equal(request.cmds[1].name, 'cockpit.navigation');
  }
});

test('late command shortcuts preserve the destination navigator presentation', () => {
  for (const kind of ['palette_open', 'sessions_open', 'machines_open', 'windows_open', 'palette_close']) {
    const [commands] = step(initialModel()[0], { kind: 'commands_open' });
    const [destination] = step(commands, { kind });
    const [loaded] = step(destination, { kind: 'keybindings_loaded', body: new Uint8Array([1, 0, 0, 0]) });
    assert.equal(loaded.navigatorView, destination.navigatorView, kind);
    assert.equal(loaded.paletteOpen, destination.paletteOpen, kind);
    assert.deepEqual(loaded.paletteNotice, destination.paletteNotice, kind);
    assert.equal(loaded.paletteCursor, destination.paletteCursor, kind);
    assert.equal(loaded.paletteLoading, destination.paletteLoading, kind);
  }
});
