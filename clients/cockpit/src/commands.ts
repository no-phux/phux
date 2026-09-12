import { asciiBytes } from "@native-sdk/core";
import { COMMAND_CATALOG, type CommandDefinition } from "./command-catalog.ts";
import { type KeybindingPage, keybindingHint } from "./keybindings.ts";

export interface ActionRow {
  readonly index: number;
  readonly label: Uint8Array;
  readonly shortcut: Uint8Array;
  readonly detail: Uint8Array;
  readonly highlighted: boolean;
  readonly disabled: boolean;
}

function fold(byte: number): number {
  return byte >= 65 && byte <= 90 ? byte + 32 : byte;
}

export function containsQuery(text: Uint8Array, query: Uint8Array): boolean {
  if (query.length === 0) return true;
  for (let start = 0; start + query.length <= text.length; start += 1) {
    let matches = true;
    for (let at = 0; at < query.length; at += 1) {
      if (fold(text[start + at]) !== fold(query[at])) matches = false;
    }
    if (matches) return true;
  }
  return false;
}

export function commandDefinition(index: number): CommandDefinition | null {
  for (const command of COMMAND_CATALOG) {
    if (command.index === index) return command;
  }
  return null;
}

export function terminalCommand(name: string): boolean {
  for (const command of ["terminal.close", "terminal.copy", "terminal.paste", "terminal.select-all", "terminal.clear", "terminal.find", "terminal.find-next", "terminal.find-previous", "pane.split-right", "pane.split-down", "pane.previous", "pane.next", "tab.previous", "tab.next", "tab.move-left", "tab.move-right", "directory.open"]) {
    if (name === command) return true;
  }
  return false;
}

export function contextualCommand(name: string): boolean {
  if (terminalCommand(name)) return true;
  if (name === "window.minimize" || name === "window.fullscreen") return true;
  return name === "terminal.new" || name === "window.new" || name === "session.new" || name === "session.rename";
}

export function commandRows(query: Uint8Array, cursor: number, hasTerminal: boolean, context: Uint8Array, bindings: KeybindingPage): readonly ActionRow[] {
  const rows: ActionRow[] = [];
  for (const command of COMMAND_CATALOG) {
    if (!containsQuery(command.label, query)) continue;
    const disabled = terminalCommand(command.name) && !hasTerminal;
    const index = command.index >= 0 && command.index <= 65535 ? Math.trunc(command.index) : 0;
    rows.push({ index, label: command.label, shortcut: keybindingHint(bindings, asciiBytes(command.name)),
      detail: disabled ? asciiBytes("Requires a focused terminal") : context,
      disabled, highlighted: rows.length === cursor });
  }
  return rows;
}
