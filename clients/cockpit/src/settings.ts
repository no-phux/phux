import { asciiBytes } from "@native-sdk/core";
import type { Appearance } from "./appearance.ts";

export interface Setting {
  readonly id: number;
  readonly section: number;
  readonly label: Uint8Array;
  readonly defaultLabel: Uint8Array;
  readonly applicability: Uint8Array;
  readonly timing: Uint8Array;
  readonly editable: boolean;
  readonly value: Uint8Array;
  readonly effectiveValue: Uint8Array;
}

function setting(id: number, section: number, label: string, defaults: string, applies: string, timing: string, editable: boolean): Setting {
  return { id: id >= 0 && id <= 13 ? Math.trunc(id) : 0,
    section: section >= 0 && section <= 4 ? Math.trunc(section) : 0,
    label: asciiBytes(label), defaultLabel: asciiBytes(defaults),
    applicability: asciiBytes(applies), timing: asciiBytes(timing), editable, value: new Uint8Array(0), effectiveValue: new Uint8Array(0) };
}

/** Display descriptions have one owner. IDs match the append-only native schema. */
export function settingsCatalog(): readonly Setting[] {
  return [
    setting(0, 0, "Font family", "JetBrains Mono NL Nerd Font Mono (bundled)", "All terminal views. Blank restores the bundled face; Geist Mono selects the other shipped face. Other fonts are unsupported.", "Live preview", true),
    setting(1, 0, "Font size", "13 pt", "All terminal views. 4 to 72 points.", "Live preview", true),
    setting(2, 0, "Theme / follow system", "Cockpit default", "Use auto to follow macOS. Explicit foreground/background take precedence.", "Live preview", true),
    setting(3, 0, "Minimum contrast", "3", "All Cockpit terminal views, including Phux. Changes presentation without changing source colors. 1 disables the floor; 21 is maximum.", "Live preview", true),
    setting(4, 1, "Cursor style", "block", "Scratch terminal default: block, bar, underline. Phux and terminal applications own their cursors.", "Live scratch preview", true),
    setting(5, 1, "Cursor blink", "true", "Scratch terminal default. Phux and terminal applications own their cursors.", "Live scratch preview", true),
    setting(6, 1, "Scrollback retention (bytes)", "52428800 (50 MiB)", "New scratch terminals only. Phux history is owned by the serving machine.", "New scratch terminals", true),
    setting(7, 1, "Shell command", "/bin/zsh (login environment)", "New scratch terminals on This Mac. Phux shells use the serving user's configuration.", "New scratch terminals", true),
    setting(8, 1, "Inherit working directory", "true", "New tabs and splits inherit from the focused resource on its machine.", "New terminals", true),
    setting(9, 3, "Tab placement", "top", "Choose top or side. All Cockpit windows.", "Live preview", true),
    setting(10, 1, "Preferred editor", "VISUAL, then EDITOR", "Local configuration editor command and arguments. Blank uses environment discovery.", "Next editor launch", true),
    setting(11, 2, "Keyboard shortcuts", "Shipping Cockpit commands", "Remap and reset actual Cockpit bindings below. Use Cmd-based chords or none; conflicts are checked before applying.", "Live preview; persisted on Save", false),
    setting(12, 4, "Phux destination / session", "Resolved environment, config, or platform default", "Read-only connection context below. Use Machines and Sessions to change work.", "Explicit connection action", false),
    setting(13, 4, "Phux server and TUI preferences", "Serving machine configuration", "Shell and history for Phux panes belong to that machine. Run phux config path there, then edit that file; TUI Settings edits TUI preferences.", "Owner-specific; server defaults on server start", false),
  ];
}

function folded(byte: number): number {
  return byte >= 65 && byte <= 90 ? byte + 32 : byte;
}

function contains(text: Uint8Array, query: Uint8Array): boolean {
  for (let start = 0; start + query.length <= text.length; start += 1) {
    let at = 0;
    while (at < query.length && folded(text[start + at]) === folded(query[at])) at += 1;
    if (at === query.length) return true;
  }
  return false;
}

function matches(row: Setting, query: Uint8Array): boolean {
  return contains(row.label, query) || contains(row.applicability, query);
}

function withValue(row: Setting, value: Uint8Array): Setting {
  const id = row.id;
  const section = row.section;
  const effectiveValue = effectiveSettingValue(row, value);
  return { ...row, id: id >= 0 && id <= 13 ? Math.trunc(id) : 0,
    section: section >= 0 && section <= 4 ? Math.trunc(section) : 0,
    value, effectiveValue };
}

function sameText(value: Uint8Array, expected: string): boolean {
  const bytes = asciiBytes(expected);
  return value.length === bytes.length && contains(value, bytes);
}

function effectiveSettingValue(row: Setting, value: Uint8Array): Uint8Array {
  if (value.length === 0) return row.defaultLabel;
  if (row.id !== 0) return value;
  if (sameText(value, "Geist Mono")) return asciiBytes("Geist Mono");
  if (sameText(value, "JetBrains Mono NL Nerd Font Mono")) return row.defaultLabel;
  return asciiBytes("Bundled face; requested family is unsupported");
}

export function settingsRows(appearance: Appearance, query: Uint8Array, section: number): readonly Setting[] {
  const rows: Setting[] = [];
  for (const row of settingsCatalog()) {
    if (query.length === 0 && row.section !== section) continue;
    if (query.length > 0 && !matches(row, query)) continue;
    const value = row.id < appearance.values.length ? appearance.values[row.id] : new Uint8Array(0);
    rows.push(withValue(row, value));
  }
  return rows;
}

export function settingRequest(id: number, value: Uint8Array): Uint8Array {
  const payload = new Uint8Array(3 + value.length);
  payload[0] = 2; payload[1] = 8; payload[2] = id;
  payload.set(value, 3);
  return payload;
}

export function resetSettingRequest(id: number): Uint8Array {
  return new Uint8Array([2, 9, id]);
}

export function reloadSettingsRequest(): Uint8Array {
  return new Uint8Array([2, 10, 0]);
}
