import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { initialModel, update } from '../core.ts';
import { appearanceResponse } from '../appearance.ts';

const text = value => new TextDecoder().decode(value);
const bytes = value => new TextEncoder().encode(value);
const step = (model, msg) => {
  const result = update(model, msg);
  return Array.isArray(result) ? result : [result, null];
};
function reply({ active = true, outcome = 0, dirty = false, theme = 0, cursor = 0, placement = 0 } = {}) {
  const font = bytes('14.0 pt');
  const contrast = bytes('15.2:1 text contrast');
  return new Uint8Array([1, +active, outcome, +dirty, theme, cursor, placement, 0, font.length, contrast.length, ...font, ...contrast]);
}
function opened() {
  const model = step(initialModel()[0], { kind: 'settings_open' })[0];
  return step(model, { kind: 'appearance_loaded', body: reply() })[0];
}
function keybindingReply(binding = 'Cmd+Shift+t', defaultBinding = 'Cmd+t') {
  const command = bytes('terminal.new');
  const label = bytes('New Tab');
  const current = bytes(binding);
  const fallback = bytes(defaultBinding);
  return new Uint8Array([1, 1, 0, 0, 0, +(binding !== defaultBinding), command.length, label.length,
    current.length, fallback.length, ...command, ...label, ...current, ...fallback]);
}

test('appearance response rejects malformed lengths and flags', () => {
  const valid = reply();
  assert.equal(text(appearanceResponse(valid).fontLabel), '14.0 pt');
  for (let i = 0; i < valid.length; ++i) assert.equal(appearanceResponse(valid.slice(0, i)), null);
  for (const at of [1, 2, 3, 5, 6, 7]) {
    const malformed = valid.slice(); malformed[at] = 255;
    assert.equal(appearanceResponse(malformed), null);
  }
});

test('opening admits a transaction and blocks preview changes until its reply', () => {
  const [model, cmd] = step(initialModel()[0], { kind: 'settings_open' });
  assert.equal(model.mainSettingsOpen, true);
  assert.equal(model.appearanceBusy, true);
  assert.equal(cmd.cmds[0].name, 'cockpit.committed');
  assert.equal(cmd.cmds.at(-1).name, 'cockpit.appearance');
  assert.deepEqual([...cmd.cmds.at(-1).payload], [1, 0, 0]);
  assert.deepEqual(step(model, { kind: 'settings_font', direction: 1 }), [model, null]);
});

test('Save stays open until confirmed; a refused write preserves Cancel', () => {
  let [model, cmd] = step(opened(), { kind: 'settings_commit' });
  assert.equal(model.settingsOpen, true);
  assert.deepEqual([...cmd.payload], [1, 7, 0]);
  [model] = step(model, { kind: 'appearance_loaded', body: reply({ outcome: 3, dirty: true }) });
  assert.equal(model.settingsOpen, true);
  assert.match(text(model.appearance.notice), /Could not save/);
  [model, cmd] = step(model, { kind: 'settings_close' });
  assert.deepEqual([...cmd.payload], [1, 6, 0]);
  assert.equal(model.settingsOpen, true);
  [model, cmd] = step(model, { kind: 'appearance_loaded', body: reply({ active: false, outcome: 2 }) });
  assert.equal(model.settingsOpen, false);
  assert.equal(cmd.name, 'cockpit.committed');
});

test('opening navigation from a preview waits for rollback before showing results', () => {
  let [model, cmd] = step(opened(), { kind: 'palette_open' });
  assert.equal(model.paletteOpen, false);
  assert.equal(model.navigationAfterSettings, true);
  assert.deepEqual([...cmd.payload], [1, 6, 0]);
  [model, cmd] = step(model, { kind: 'appearance_loaded', body: reply({ active: false, outcome: 2 }) });
  assert.equal(model.settingsOpen, false);
  assert.equal(model.paletteOpen, true);
  assert.equal(cmd.cmds[0].name, 'cockpit.committed');
});

test('a failed rollback still dismisses Settings and releases the keyboard', () => {
  let [model, cmd] = step(opened(), { kind: 'settings_close' });
  assert.deepEqual([...cmd.payload], [1, 6, 0]);
  [model, cmd] = step(model, { kind: 'appearance_failed', error: bytes('engine unavailable') });
  assert.equal(model.settingsOpen, false);
  assert.equal(model.mainSettingsOpen, false);
  assert.equal(model.appearanceBusy, false);
  assert.deepEqual(cmd, { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  // A failed rollback on the way to the switcher still reaches the switcher.
  [model] = step(opened(), { kind: 'palette_open' });
  [model, cmd] = step(model, { kind: 'appearance_failed', error: bytes('engine unavailable') });
  assert.equal(model.settingsOpen, false);
  assert.equal(model.paletteOpen, true);
  assert.ok(cmd.cmds.some(effect => effect.name === 'cockpit.navigation'));
});

test('repeated Cancel during rollback emits one rollback request', () => {
  let [model, cmd] = step(opened(), { kind: 'settings_close' });
  assert.equal(model.appearanceClosing, true);
  assert.equal(model.settingsOpen, true);
  assert.deepEqual([...cmd.payload], [1, 6, 0]);
  const [again, againCmd] = step(model, { kind: 'settings_close' });
  assert.equal(again.appearanceClosing, true);
  assert.equal(again.settingsOpen, true);
  assert.equal(againCmd, null);
  const [fromNav, navCmd] = step(again, { kind: 'palette_open' });
  assert.equal(fromNav.navigationAfterSettings, true);
  assert.equal(navCmd, null);
});

test('without a native transaction Cancel closes locally, while a failed Save keeps the preview', () => {
  let [model] = step(initialModel()[0], { kind: 'settings_open' });
  [model] = step(model, { kind: 'appearance_failed', error: bytes('engine unavailable') });
  assert.equal(model.settingsOpen, true);
  assert.match(text(model.appearance.notice), /unavailable/);
  let cmd;
  [model, cmd] = step(model, { kind: 'settings_close' });
  assert.equal(model.settingsOpen, false);
  assert.equal(cmd.name, 'cockpit.committed');
  // A failed Save is not a dismissal: the active preview stays cancellable.
  [model] = step(opened(), { kind: 'settings_commit' });
  [model] = step(model, { kind: 'appearance_failed', error: bytes('write failed') });
  assert.equal(model.settingsOpen, true);
  assert.equal(model.appearanceBusy, false);
});

test('the placement menu command previews through the appearance transaction', () => {
  const model = opened();
  const [previewing, cmd] = step(model, { kind: 'toggle_tab_placement' });
  assert.equal(cmd.name, 'cockpit.appearance');
  assert.deepEqual([...cmd.payload], [1, 5, 1]);
  assert.equal(previewing.tabPlacement, model.tabPlacement);
  assert.deepEqual(step(previewing, { kind: 'toggle_tab_placement' }), [previewing, null]);
  const [rail] = step(previewing, { kind: 'appearance_loaded', body: reply({ placement: 1, dirty: true }) });
  assert.deepEqual([...step(rail, { kind: 'toggle_tab_placement' })[1].payload], [1, 5, 0]);
  // Outside Settings the command keeps its direct intent.
  const [, direct] = step(initialModel()[0], { kind: 'toggle_tab_placement' });
  assert.equal(direct.name, 'cockpit.intent');
});

test('Workspace and Connection arrow keys never change a hidden theme', () => {
  for (const section of [1, 2, 4]) {
    const model = step(opened(), { kind: 'settings_section', section })[0];
    assert.deepEqual(step(model, { kind: 'settings_move', delta: 1 }), [model, null]);
  }
});

test('About is a reachable Settings group', () => {
  const [model] = step(opened(), { kind: 'settings_section', section: 5 });
  assert.equal(model.settingsSection, 5);
  assert.equal(model.settingEditId, 65535);
});

test('Settings details open one accordion at a time and keep the editor outside', () => {
  let [model] = step(opened(), { kind: 'settings_detail', id: 0 });
  assert.equal(model.settingsDetailId, 0);
  [model] = step(model, { kind: 'settings_detail', id: 1 });
  assert.equal(model.settingsDetailId, 1);
  [model] = step(model, { kind: 'settings_select', id: 1 });
  assert.equal(model.settingEditId, 1);
  assert.equal(model.settingsDetailId, 1);
  [model] = step(model, { kind: 'settings_detail', id: 1 });
  assert.equal(model.settingsDetailId, 65535);
  assert.equal(model.settingEditId, 1);
});

test('search reveals concealed matching details without using a search-field', () => {
  const [hidden] = step(opened(), { kind: 'settings_query', edit: { kind: 'insert_text', text: bytes('Geist Mono') } });
  assert.deepEqual(hidden.settingRows.map(row => row.id), [0]);
  assert.equal(hidden.settingsDetailId, 0);
  assert.match(text(hidden.settingRows[0].applicability), /Geist Mono/);
  const [timing] = step(opened(), { kind: 'settings_query', edit: { kind: 'insert_text', text: bytes('Live preview') } });
  assert.ok(timing.settingRows.some(row => row.id === 0));
  assert.equal(timing.settingsDetailId, 65535);
});

test('Settings search sanitizes an invalid detail identity at the model boundary', () => {
  const corrupt = { ...opened(), settingsDetailId: Number.NaN };
  const [model] = step(corrupt, { kind: 'settings_query', edit: { kind: 'insert_text', text: bytes('Live preview') } });
  assert.equal(model.settingsDetailId, 65535);
});

test('keyboard search reveals a concealed default chord across Settings groups', () => {
  const [loaded] = step(opened(), { kind: 'keybindings_loaded', body: keybindingReply() });
  assert.equal(loaded.showBindingRows, false);
  const [matched] = step(loaded, { kind: 'settings_query', edit: { kind: 'insert_text', text: bytes('Cmd+t') } });
  assert.equal(matched.showBindingRows, true);
  assert.equal(matched.bindingRows.length, 1);
  assert.equal(matched.bindingDetailIndex, 0);
});

test('Settings groups cannot retain keyboard rows or share the connection accordion identity', () => {
  const [loaded] = step(opened(), { kind: 'keybindings_loaded', body: keybindingReply() });
  const [keyboard] = step(loaded, { kind: 'settings_section', section: 2 });
  assert.equal(keyboard.showBindingRows, true);
  const [connection] = step(keyboard, { kind: 'settings_section', section: 4 });
  assert.equal(connection.showBindingRows, false);
  assert.deepEqual(connection.settingRows.map(row => row.id), []);

  const markup = readFileSync(new URL('../windows/components/cockpit-settings.native', import.meta.url), 'utf8');
  assert.equal(markup.match(/on-toggle="settings_detail:12"/g)?.length ?? 0, 0);
  assert.equal(markup.match(/setting\.id == 12/g)?.length ?? 0, 0);
  assert.match(markup, /accordion text="Details" label="\{setting\.label\}"/);
  assert.match(markup, /accordion text="Details" label="\{binding\.label\}"/);
  assert.match(markup, /if test="\{settingsFooterSave\}"/);
});

test('Connection and About are read-only while editable groups keep Save', () => {
  const openedSettings = opened();
  assert.equal(text(openedSettings.settingsSections[4].label), 'Connection');
  assert.equal(openedSettings.settingsFooterSave, true);
  const [connection] = step(openedSettings, { kind: 'settings_section', section: 4 });
  assert.equal(connection.settingsFooterSave, false);
  assert.equal(connection.noSettingRows, false);
  const [about] = step(openedSettings, { kind: 'settings_section', section: 5 });
  assert.equal(about.settingsFooterSave, false);
  const [keyboard] = step(openedSettings, { kind: 'settings_section', section: 2 });
  assert.equal(keyboard.settingsFooterSave, true);
  const dirty = step(openedSettings, { kind: 'appearance_loaded', body: reply({ dirty: true }) })[0];
  const [dirtyConnection] = step(dirty, { kind: 'settings_section', section: 4 });
  assert.equal(dirtyConnection.appearance.dirty, true);
  assert.equal(dirtyConnection.settingsFooterSave, false);
  const [fromConnection, cmd] = step(connection, { kind: 'sessions_open' });
  assert.equal(fromConnection.settingsOpen, true);
  assert.equal(fromConnection.paletteOpen, false);
  assert.deepEqual([...cmd.payload], [1, 6, 0]);
});

test('global search results stay disclosure-only in read-only Settings groups', () => {
  let [model] = step(opened(), { kind: 'settings_section', section: 4 });
  [model] = step(model, { kind: 'settings_query', edit: { kind: 'insert_text', text: bytes('Geist Mono') } });
  assert.deepEqual(model.settingRows.map(row => row.id), [0]);
  const [setting] = step(model, { kind: 'settings_select', id: 0 });
  assert.equal(setting.settingEditId, 65535);

  [model] = step(opened(), { kind: 'keybindings_loaded', body: keybindingReply() });
  [model] = step(model, { kind: 'settings_section', section: 5 });
  [model] = step(model, { kind: 'settings_query', edit: { kind: 'insert_text', text: bytes('Cmd+t') } });
  assert.equal(model.showBindingRows, true);
  const [binding] = step(model, { kind: 'binding_select', index: 0 });
  assert.equal(binding.bindingEditIndex, 65535);
});
