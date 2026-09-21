import { test } from 'node:test';
import assert from 'node:assert/strict';
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
function replyV2({ values = ['', '14', 'phux-dark', '3', 'block', 'true', '52428800', '', 'true', 'top', ''], ...header } = {}) {
  const base = reply(header);
  base[0] = 2;
  const records = values.flatMap((value, id) => [id, bytes(value).length, 0, ...bytes(value)]);
  return new Uint8Array([...base, 0, ...records]);
}
function opened(body = reply()) {
  const model = step(initialModel()[0], { kind: 'settings_open' })[0];
  return step(model, { kind: 'appearance_loaded', body })[0];
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

test('direct settings controls request one authoritative setting update', () => {
  let model = step(opened(replyV2()), { kind: 'settings_section', section: 1 })[0];
  let cmd;
  [model] = step(model, { kind: 'settings_select', id: 7 });
  [model] = step(model, { kind: 'settings_value', edit: { kind: 'insert_text', text: bytes('/bin/fish') } });
  const draftAnchor = model.settingAnchor;
  const draftFocus = model.settingFocus;
  [model, cmd] = step(model, { kind: 'settings_disable', id: 5 });
  assert.deepEqual([...cmd.payload], [2, 8, 5, 102, 97, 108, 115, 101]);
  assert.equal(model.appearanceBusy, true);
  assert.deepEqual(step(model, { kind: 'settings_enable', id: 5 }), [model, null]);
  assert.equal(model.settingRows.find(row => row.id === 5).checked, true);
  [model, cmd] = step(model, { kind: 'appearance_loaded', body: replyV2({ cursor: 1, placement: 1,
    values: ['', '14', 'phux-dark', '3', 'bar', 'false', '52428800', '', 'true', 'side', ''] }) });
  assert.equal(model.appearanceBusy, false);
  assert.equal(text(model.settingEditValue), '/bin/fish');
  assert.equal(model.settingAnchor, draftAnchor);
  assert.equal(model.settingFocus, draftFocus);
  assert.equal(model.settingRows.find(row => row.id === 5).checked, false);
  assert.equal(model.cursorChoices[1].selected, true);
  assert.equal(model.placementChoices[1].selected, true);
  [model] = step(model, { kind: 'settings_section', section: 0 });
  [model, cmd] = step(model, { kind: 'settings_font_family', index: 1 });
  assert.deepEqual([...cmd.payload], [2, 8, 0, 71, 101, 105, 115, 116, 32, 77, 111, 110, 111]);
  assert.equal(model.appearanceBusy, true);
  assert.deepEqual(step(model, { kind: 'settings_font_family', index: 0 }), [model, null]);
  assert.deepEqual(step(model, { kind: 'settings_font_family', index: 2 }), [model, null]);
  [model] = step(model, { kind: 'appearance_failed', error: bytes('rejected') });
  assert.equal(model.appearanceBusy, false);
  assert.equal(text(model.settingEditValue), '/bin/fish');
  const hidden = step(model, { kind: 'settings_section', section: 1 })[0];
  const searched = step(hidden, { kind: 'settings_query', edit: { kind: 'insert_text', text: bytes('cursor') } })[0];
  assert.deepEqual(step(searched, { kind: 'settings_font_family', index: 1 }), [searched, null]);
  const appearanceSection = step(model, { kind: 'settings_section', section: 0 })[0];
  assert.deepEqual(step(appearanceSection, { kind: 'settings_cursor', index: 1 }), [appearanceSection, null]);
  assert.deepEqual(step(model, { kind: 'settings_disable', id: 99 }), [model, null]);
});

test('direct controls refuse closed, busy, hidden and search-filtered actions', () => {
  const actions = [
    { section: 0, action: { kind: 'settings_font_family', index: 1 } },
    { section: 0, action: { kind: 'settings_font', direction: 1 } },
    { section: 1, action: { kind: 'settings_cursor', index: 1 } },
    { section: 1, action: { kind: 'settings_disable', id: 5 } },
    { section: 1, action: { kind: 'settings_enable', id: 8 } },
    { section: 3, action: { kind: 'settings_placement', index: 1 } },
    { section: 0, action: { kind: 'settings_reset', id: 0 } },
  ];
  for (const { section, action } of actions) {
    const visible = step(opened(replyV2()), { kind: 'settings_section', section })[0];
    assert.ok(step(visible, action)[1], `${action.kind} admits visible action`);
    const hidden = step(visible, { kind: 'settings_section', section: 4 })[0];
    const filtered = step(visible, { kind: 'settings_query', edit: { kind: 'insert_text', text: bytes('no matching setting') } })[0];
    for (const refused of [initialModel()[0], { ...visible, appearanceBusy: true }, hidden, filtered]) {
      assert.deepEqual(step(refused, action), [refused, null], action.kind);
    }
  }
  let draft = step(opened(replyV2()), { kind: 'settings_select', id: 3 })[0];
  assert.ok(step(draft, { kind: 'settings_apply' })[1]);
  [draft] = step(draft, { kind: 'settings_section', section: 3 });
  assert.deepEqual(step(draft, { kind: 'settings_apply' }), [draft, null]);
});

test('legacy responses cannot guess a font-family or boolean value', () => {
  const model = opened();
  assert.deepEqual(step(model, { kind: 'settings_font_family', index: 1 }), [model, null]);
  const terminal = step(model, { kind: 'settings_section', section: 1 })[0];
  for (const id of [5, 8]) {
    assert.equal(terminal.settingRows.find(row => row.id === id).available, false);
    assert.deepEqual(step(terminal, { kind: 'settings_enable', id }), [terminal, null]);
    assert.deepEqual(step(terminal, { kind: 'settings_disable', id }), [terminal, null]);
  }
  for (const id of [6, 7, 10]) {
    const row = terminal.settingRows.find(row => row.id === id);
    assert.equal(text(row.effectiveValue), 'Current value unavailable');
    assert.deepEqual(step(terminal, { kind: 'settings_select', id }), [terminal, null]);
    assert.deepEqual(step(terminal, { kind: 'settings_reset', id }), [terminal, null]);
    const staleDraft = { ...terminal, settingEditId: id, settingEditValue: bytes('old draft') };
    assert.deepEqual(step(staleDraft, { kind: 'settings_apply' }), [staleDraft, null]);
  }
});

test('native bundled-font spellings and unsupported families project honest choices', () => {
  for (const [family, expected] of [['', [true, false]], ['JetBrains Mono NL Nerd Font Mono', [true, false]],
    ['Geist Mono', [false, true]], ['Unknown Font', [false, false]]]) {
    const values = ['', '14', 'phux-dark', '3', 'block', 'true', '52428800', '', 'true', 'top', ''];
    values[0] = family;
    const model = opened(replyV2({ values }));
    assert.deepEqual(model.fontChoices.map(choice => choice.selected), expected, family);
    assert.equal(text(model.appearance.values[0]), family);
  }
});

test('boolean choices request the explicit value even when it is already current', () => {
  const model = step(opened(replyV2()), { kind: 'settings_section', section: 1 })[0];
  for (const id of [5, 8]) {
    assert.deepEqual([...step(model, { kind: 'settings_enable', id })[1].payload], [2, 8, id, ...bytes('true')]);
    assert.deepEqual([...step(model, { kind: 'settings_disable', id })[1].payload], [2, 8, id, ...bytes('false')]);
  }
});

test('clean Save remains available in editable sections and closes on native confirmation', () => {
  let [model, cmd] = step(opened(), { kind: 'settings_commit' });
  assert.deepEqual([...cmd.payload], [1, 7, 0]);
  assert.equal(model.settingsOpen, true);
  [model] = step(model, { kind: 'appearance_loaded', body: reply({ active: false, outcome: 1 }) });
  assert.equal(model.settingsOpen, false);
});

test('Save stays open until confirmed; a refused write preserves Cancel', () => {
  let [model] = step(opened(), { kind: 'appearance_loaded', body: reply({ dirty: true }) });
  let cmd;
  [model, cmd] = step(model, { kind: 'settings_commit' });
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
  [model] = step(opened(), { kind: 'appearance_loaded', body: reply({ dirty: true }) });
  [model] = step(model, { kind: 'settings_commit' });
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
    assert.deepEqual(step(model, { kind: 'settings_pick', index: 0 }), [model, null]);
  }
});

test('Connection and About are read-only; editable groups keep Save', () => {
  const openedSettings = opened();
  assert.equal(text(openedSettings.settingsSections[4].label), 'Connection');
  assert.equal(openedSettings.settingsFooterSave, true);
  const [connection] = step(openedSettings, { kind: 'settings_section', section: 4 });
  assert.equal(connection.settingsSection, 4);
  assert.deepEqual(connection.settingRows.map(row => row.id), []);
  assert.equal(connection.settingsFooterSave, false);
  assert.deepEqual(step(connection, { kind: 'settings_commit' }), [connection, null]);
  assert.equal(connection.noSettingRows, false);
  const [about] = step(openedSettings, { kind: 'settings_section', section: 5 });
  assert.equal(about.settingsFooterSave, false);
  const [keyboard] = step(openedSettings, { kind: 'settings_section', section: 2 });
  assert.equal(keyboard.settingsFooterSave, true);
  const dirty = step(openedSettings, { kind: 'appearance_loaded', body: reply({ dirty: true }) })[0];
  assert.equal(dirty.appearance.dirty, true);
  assert.equal(dirty.settingsFooterSave, true);
  const [dirtyConnection] = step(dirty, { kind: 'settings_section', section: 4 });
  assert.equal(dirtyConnection.appearance.dirty, true);
  assert.equal(dirtyConnection.settingsFooterSave, true);
  const [dirtyAbout] = step(dirty, { kind: 'settings_section', section: 5 });
  assert.equal(dirtyAbout.settingsFooterSave, true);
  for (const status of [dirtyConnection, dirtyAbout]) {
    assert.deepEqual([...step(status, { kind: 'settings_commit' })[1].payload], [1, 7, 0]);
    assert.deepEqual([...step(status, { kind: 'settings_close' })[1].payload], [1, 6, 0]);
  }
  const [fromConnection, cmd] = step(connection, { kind: 'sessions_open' });
  assert.equal(fromConnection.settingsOpen, true);
  assert.equal(fromConnection.paletteOpen, false);
  assert.deepEqual([...cmd.payload], [1, 6, 0]);
});
