import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialModel, update } from '../core.ts';
import { readFileSync } from 'node:fs';

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


test('header recovery cannot reconnect another window or an already recovered provider', () => {
  const intents = command => (command?.op === 'batch' ? command.cmds : [command])
    .filter(effect => effect?.name === 'cockpit.intent');
  const [opened] = step({ ...initialModel()[0], engineConnected: true, canReconnect: true,
    activeWindow: 2, window2Open: true }, { kind: 'header_hosts_toggle' });
  const [reconnecting, accepted] = step(opened, { kind: 'header_reconnect' });
  assert.equal(reconnecting.headerMenuWindow, -1);
  assert.equal(intents(accepted).length, 1);
  assert.equal(intents(accepted)[0].payload[1], 12);
  const [, otherWindow] = step({ ...opened, activeWindow: 1, window1Open: true }, { kind: 'header_reconnect' });
  assert.equal(intents(otherWindow).length, 0);
  const [, recovered] = step({ ...opened, canReconnect: false }, { kind: 'header_reconnect' });
  assert.equal(intents(recovered).length, 0);
});

test('the location trigger menu reaches Sessions as well as Machines and asks for nothing itself', () => {
  const [opened, command] = step({ ...initialModel()[0], engineConnected: true }, { kind: 'header_hosts_toggle' });
  assert.equal(opened.headerHosts, true);
  assert.equal(opened.mainHeaderMenuOpen, true);
  assert.deepEqual(command, { op: 'host_bytes', name: 'cockpit.committed', payload: new Uint8Array() });
  const [sessions] = step(opened, { kind: 'header_sessions' });
  assert.equal(sessions.headerMenuWindow, -1);
  assert.equal(sessions.paletteOpen, true);
  assert.equal(sessions.navigatorView, 0);
  const [machines] = step(opened, { kind: 'header_machines' });
  assert.equal(machines.navigatorView, 2);
});

test('the workspace menu holds window and app actions only; the location menu holds Sessions and Machines', () => {
  const read = path => readFileSync(new URL(path, import.meta.url), 'utf8');
  const window = read('../windows/components/cockpit-window.native');
  const actions = window.slice(window.indexOf('label="Workspace actions">'), window.indexOf('</dropdown-menu>'));
  for (const gone of ['header_sessions', 'header_machines', 'header_windows', 'header_agents', '{title}', '{detail}']) {
    assert.doesNotMatch(actions, new RegExp(gone.replace(/[{}]/g, '\\$&')), gone);
  }
  const location = read('../windows/components/cockpit-connected-hosts.native');
  assert.match(location, /on-press="header_sessions">Sessions…</);
  assert.match(location, /on-press="header_machines">Machines…</);
});
