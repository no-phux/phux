import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialModel, update } from '../core.ts';

const step = (model, message) => {
  const result = update(model, message);
  return Array.isArray(result) ? result : [result, null];
};

test('workspace menu is scoped to its opening window and commits input ownership', () => {
  const initial = { ...initialModel()[0], activeWindow: 2, window2Open: true };
  const [opened, command] = step(initial, { kind: 'header_menu_toggle' });
  assert.equal(initial.window2HeaderMenuOpen, false);
  assert.equal(opened.window2HeaderMenuOpen, true);
  assert.equal(opened.mainHeaderMenuOpen, false);
  assert.equal(command.name, 'cockpit.committed');
  const [closed, resumed] = step(opened, { kind: 'header_menu_close' });
  assert.equal(closed.headerMenuWindow, -1);
  assert.equal(closed.window2HeaderMenuOpen, false);
  assert.equal(resumed.name, 'cockpit.committed');
});

test('menu selection closes before handing off to the existing command', () => {
  const [opened] = step(initialModel()[0], { kind: 'header_menu_toggle' });
  const [moved, command] = step(opened, { kind: 'header_tab_placement' });
  assert.equal(moved.headerMenuWindow, -1);
  assert.equal(moved.tabPlacement, 'side');
  assert.equal(command.cmds[0].name, 'cockpit.committed');
  assert.equal(command.cmds[1].name, 'cockpit.intent');
  const [machines] = step(opened, { kind: 'header_machines' });
  assert.equal(machines.headerMenuWindow, -1);
  assert.equal(machines.paletteOpen, true);
  assert.equal(machines.navigatorView, 2);
});

test('closed or other-window menu cannot execute a held item', () => {
  const initial = initialModel()[0];
  const [closed] = step(initial, { kind: 'header_tab_placement' });
  assert.equal(closed.tabPlacement, initial.tabPlacement);
  const [opened] = step(initial, { kind: 'header_menu_toggle' });
  const [changed] = step({ ...opened, activeWindow: 1 }, { kind: 'header_tab_placement' });
  assert.equal(changed.headerMenuWindow, -1);
  assert.equal(changed.tabPlacement, initial.tabPlacement);
});

test('an existing modal prevents a second surface and command shortcuts retire the menu', () => {
  const [palette] = step(initialModel()[0], { kind: 'commands_open' });
  const [refused] = step(palette, { kind: 'header_menu_toggle' });
  assert.equal(refused.headerMenuWindow, -1);
  const [opened] = step(initialModel()[0], { kind: 'header_menu_toggle' });
  const [settings, command] = step(opened, { kind: 'settings_open' });
  assert.equal(settings.headerMenuWindow, -1);
  assert.equal(settings.settingsOpen, true);
  assert.equal(command.cmds[0].name, 'cockpit.committed');
});
