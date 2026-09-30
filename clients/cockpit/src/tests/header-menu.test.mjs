import { test } from 'node:test';
import assert from 'node:assert/strict';
import { initialModel, update } from '../core.ts';
import { navigationScopedRequest } from '../protocol.ts';

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

const hostTarget = new Uint8Array([6, 2, ...new Array(32).fill(0)]);
const hostsReply = model => {
  const header = navigationScopedRequest(model.engineRevision, 0, new Uint8Array(0), 6, new Uint8Array(0));
  const label = new TextEncoder().encode('build');
  const detail = new TextEncoder().encode('Connected');
  return new Uint8Array([...header, 1, 0, 1, 0, 0, label.length, 34, 0, ...hostTarget, ...label, 0x4e, 3, 3, detail.length, ...detail]);
};

test('host dropdown opens exact captured attachment sessions in its owning window', () => {
  let [model] = step({ ...initialModel()[0], engineConnected: true, activeWindow: 2, window2Open: true }, { kind: 'header_hosts_toggle' });
  [model] = step(model, { kind: 'navigation_loaded', body: hostsReply(model) });
  assert.equal(model.paletteRows[0].current, true);
  const target = model.paletteRows[0].target;
  [model] = step(model, { kind: 'header_host_pick', target });
  assert.equal(model.headerMenuWindow, -1);
  assert.equal(model.window2PaletteOpen, true);
  assert.equal(model.navigatorView, 1);
  assert.equal(model.paletteScope, 7);
  assert.deepEqual(model.paletteHost, target);
});

test('held host rows and late list replies cannot follow focus or a dismissed dropdown', () => {
  let [opened] = step({ ...initialModel()[0], engineConnected: true }, { kind: 'header_hosts_toggle' });
  [opened] = step(opened, { kind: 'navigation_loaded', body: hostsReply(opened) });
  const target = opened.paletteRows[0].target;
  const [changed, command] = step({ ...opened, activeWindow: 1, window1Open: true }, { kind: 'header_host_pick', target });
  assert.equal(command, null);
  assert.equal(changed.paletteOpen, false);
  const [closed] = step(opened, { kind: 'header_menu_close' });
  const [late] = step(closed, { kind: 'navigation_loaded', body: hostsReply(closed) });
  assert.equal(late.paletteOpen, false);
  const [reopened] = step(late, { kind: 'header_hosts_toggle' });
  const [stale, staleCommand] = step(reopened, { kind: 'header_host_pick', target });
  assert.equal(staleCommand, null, 'refreshing inventory refuses previously captured rows');
  assert.equal(stale.paletteOpen, false);
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
