/**
 * Read a Ghostty config so a Ghostty user's terminal looks and types the same
 * here: font and cell size, colours and the 16-colour palette, padding,
 * unfocused-split dimming, and keybinds mapped onto this app's commands.
 * Parsing is total: unknown keys and malformed values are ignored.
 */
import type { Theme } from "../ui/theme";
import type { DisplayPrefs } from "../workspace/persist";
import { mix } from "../ui/theme";

export interface GhosttyConfig {
  fontFamily?: string;
  fontSize?: number;
  /** Multipliers derived from `adjust-cell-width/height` percentages. */
  cellWidth?: number;
  cellHeight?: number;
  background?: string;
  foreground?: string;
  cursor?: string;
  selectionBackground?: string;
  selectionForeground?: string;
  palette: (string | undefined)[];
  paddingX?: number;
  paddingY?: number;
  unfocusedSplitOpacity?: number;
  splitDividerColor?: string;
  optionAsAlt?: boolean;
  /** Chord (this app's `cmd+ctrl+alt+shift+key` form) -> command id; "" unbinds. */
  keybinds: Map<string, string>;
  /** Chords bound with Ghostty's `global:` prefix: system-wide, even unfocused. */
  globals: Set<string>;
  /** Ghostty actions this app has no equivalent for, for the settings page. */
  unmapped: string[];
}

export function parseGhostty(text: string): GhosttyConfig {
  const config: GhosttyConfig = {
    palette: Array.from({ length: 16 }),
    keybinds: new Map(),
    globals: new Set(),
    unmapped: [],
  };
  let fontReset = false;
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const equals = line.indexOf("=");
    if (equals < 0) continue;
    const key = line.slice(0, equals).trim();
    const value = unquote(line.slice(equals + 1).trim());
    if (key === "font-family") {
      // The first family is primary; later ones are fallbacks. Empty resets.
      if (!value) {
        fontReset = true;
        delete config.fontFamily;
      } else if (!config.fontFamily || fontReset) {
        config.fontFamily = value;
        fontReset = false;
      }
      continue;
    }
    apply(config, key, value);
  }
  return config;
}

function apply(config: GhosttyConfig, key: string, value: string): void {
  switch (key) {
    case "font-size":
      setNumber(value, 4, 72, (size) => (config.fontSize = size));
      return;
    case "adjust-cell-width":
      setPercent(value, (scale) => (config.cellWidth = scale));
      return;
    case "adjust-cell-height":
      setPercent(value, (scale) => (config.cellHeight = scale));
      return;
    case "background":
    case "foreground":
    case "cursor-color":
    case "selection-background":
    case "selection-foreground":
    case "split-divider-color":
      setColor(config, key, value);
      return;
    case "palette":
      setPalette(config, value);
      return;
    case "window-padding-x":
      setNumber(firstNumber(value), 0, 64, (pad) => (config.paddingX = pad));
      return;
    case "window-padding-y":
      setNumber(firstNumber(value), 0, 64, (pad) => (config.paddingY = pad));
      return;
    case "unfocused-split-opacity":
      setNumber(value, 0.15, 1, (opacity) => (config.unfocusedSplitOpacity = opacity));
      return;
    case "macos-option-as-alt":
      config.optionAsAlt = value === "true" || value === "left" || value === "right";
      return;
    case "keybind":
      setKeybind(config, value);
      return;
  }
}

const COLOR_KEYS: Record<string, keyof GhosttyConfig> = {
  background: "background",
  foreground: "foreground",
  "cursor-color": "cursor",
  "selection-background": "selectionBackground",
  "selection-foreground": "selectionForeground",
  "split-divider-color": "splitDividerColor",
};

function setColor(config: GhosttyConfig, key: string, value: string): void {
  const color = hex(value);
  const field = COLOR_KEYS[key];
  if (!color || !field) return;
  if (field === "background") config.background = color;
  else if (field === "foreground") config.foreground = color;
  else if (field === "cursor") config.cursor = color;
  else if (field === "selectionBackground") config.selectionBackground = color;
  else if (field === "selectionForeground") config.selectionForeground = color;
  else if (field === "splitDividerColor") config.splitDividerColor = color;
}

function setPalette(config: GhosttyConfig, value: string): void {
  const match = /^(\d+)\s*=\s*(.+)$/.exec(value);
  const index = match ? Number(match[1]) : Number.NaN;
  const color = match?.[2] ? hex(match[2]) : undefined;
  if (color && index >= 0 && index < 16) config.palette[index] = color;
}

function hex(value: string): string | undefined {
  const trimmed = value.trim().toLowerCase();
  const body = trimmed.startsWith("#") ? trimmed.slice(1) : trimmed;
  if (/^[0-9a-f]{6}$/.test(body)) return `#${body}`;
  if (/^[0-9a-f]{3}$/.test(body))
    return `#${body
      .split("")
      .map((digit) => digit + digit)
      .join("")}`;
  return undefined;
}

function setNumber(value: string, low: number, high: number, set: (value: number) => void): void {
  const number = Number(value);
  if (value !== "" && Number.isFinite(number) && number >= low && number <= high) set(number);
}

/** Only percentages map onto a scale; absolute pixel adjustments are ignored. */
function setPercent(value: string, set: (scale: number) => void): void {
  const match = /^([+-]?\d+(?:\.\d+)?)%$/.exec(value);
  if (!match) return;
  const scale = 1 + Number(match[1]) / 100;
  if (scale >= 0.5 && scale <= 2) set(scale);
}

function firstNumber(value: string): string {
  return value.split(",")[0]?.trim() ?? "";
}

function unquote(value: string): string {
  return value.length >= 2 && value.startsWith('"') && value.endsWith('"')
    ? value.slice(1, -1)
    : value;
}

// ── Keybinds ─────────────────────────────────────────────────────────

const KEY_NAMES: Record<string, string> = {
  arrow_left: "left",
  arrow_right: "right",
  arrow_up: "up",
  arrow_down: "down",
  bracket_left: "[",
  bracket_right: "]",
  grave_accent: "`",
  equal: "=",
  minus: "-",
  comma: ",",
  period: ".",
  slash: "/",
  backslash: "\\",
  semicolon: ";",
  apostrophe: "'",
  return: "enter",
  page_up: "pageup",
  page_down: "pagedown",
};

const MODIFIERS: Record<string, "cmd" | "ctrl" | "alt" | "shift"> = {
  super: "cmd",
  cmd: "cmd",
  command: "cmd",
  ctrl: "ctrl",
  control: "ctrl",
  alt: "alt",
  opt: "alt",
  option: "alt",
  shift: "shift",
};

/** `super+shift+arrow_left` -> `cmd+shift+left`; undefined for sequences or unknown parts. */
export function ghosttyChord(trigger: string): string | undefined {
  const mods = new Set<string>();
  let key: string | undefined;
  for (const part of trigger.toLowerCase().split("+")) {
    const modifier = MODIFIERS[part];
    if (modifier) mods.add(modifier);
    else if (key !== undefined || !part) return undefined;
    else key = part;
  }
  if (key === undefined || key.includes(">")) return undefined;
  if (key === "plus") {
    key = "=";
    mods.add("shift");
  }
  key = KEY_NAMES[key] ?? key.replace(/^digit_/, "");
  return [...["cmd", "ctrl", "alt", "shift"].filter((mod) => mods.has(mod)), key].join("+");
}

/** Ghostty action (without its parameter where the parameter doesn't matter) -> command id. */
const ACTIONS: Record<string, string> = {
  "new_split:right": "split-right",
  "new_split:left": "split-right",
  "new_split:down": "split-down",
  "new_split:up": "split-down",
  "new_split:auto": "split-right",
  "goto_split:left": "pane-left",
  "goto_split:right": "pane-right",
  "goto_split:up": "pane-up",
  "goto_split:down": "pane-down",
  "goto_split:top": "pane-up",
  "goto_split:bottom": "pane-down",
  "goto_split:previous": "pane-prev",
  "goto_split:next": "pane-next",
  "resize_split:left": "resize-left",
  "resize_split:right": "resize-right",
  "resize_split:up": "resize-up",
  "resize_split:down": "resize-down",
  toggle_split_zoom: "zoom",
  equalize_splits: "equalize",
  close_surface: "close",
  close_tab: "close-tab",
  close_window: "close-tab",
  new_tab: "new",
  new_window: "new-window",
  previous_tab: "tab-prev",
  next_tab: "tab-next",
  last_tab: "tab-9",
  toggle_fullscreen: "fullscreen",
  increase_font_size: "font-up",
  decrease_font_size: "font-down",
  reset_font_size: "font-reset",
  scroll_page_up: "scroll-page-up",
  scroll_page_down: "scroll-page-down",
  scroll_to_top: "scroll-top",
  scroll_to_bottom: "scroll-bottom",
  clear_screen: "clear",
  open_config: "settings",
  reload_config: "reload-config",
  toggle_command_palette: "palette",
  toggle_quick_terminal: "quick-terminal",
  toggle_tab_overview: "goto",
  unbind: "",
};

/** The command a Ghostty action maps to, or undefined when there is none. */
export function ghosttyAction(action: string): string | undefined {
  const trimmed = action.trim();
  const tab = /^goto_tab:(\d)$/.exec(trimmed);
  if (tab) return `tab-${tab[1]}`;
  const [name = "", argument = ""] = trimmed.split(":");
  const direction = argument.split(",")[0] ?? "";
  return ACTIONS[`${name}:${direction}`] ?? ACTIONS[name];
}

function setKeybind(config: GhosttyConfig, value: string): void {
  if (value === "clear") {
    config.keybinds.clear();
    return;
  }
  const equals = value.lastIndexOf("=");
  if (equals < 0) return;
  // Prefixes like `global:` and `unconsumed:` change delivery, not the chord.
  const raw = value.slice(0, equals);
  const global = /^((all|unconsumed|performable):)*global:/.test(raw);
  const trigger = raw.replace(/^((global|all|unconsumed|performable):)+/, "");
  const action = value.slice(equals + 1);
  const chord = ghosttyChord(trigger);
  const command = ghosttyAction(action);
  if (!chord) return;
  if (command === undefined) {
    config.unmapped.push(`${trigger} = ${action}`);
    return;
  }
  config.keybinds.set(chord, command);
  if (global) config.globals.add(chord);
  else config.globals.delete(chord);
}

// ── Theme ────────────────────────────────────────────────────────────

/** A theme from Ghostty's colours, or undefined when it names none. */
export function ghosttyTheme(config: GhosttyConfig): Theme | undefined {
  const background = config.background;
  const foreground = config.foreground;
  if (!background || !foreground) return undefined;
  const palette = config.palette.every((entry) => entry !== undefined)
    ? config.palette.flatMap((entry) => (entry ? [entry] : []))
    : undefined;
  const theme: Theme = {
    id: "ghostty",
    name: "Ghostty",
    appearance: luminance(background) < 0.4 ? "dark" : "light",
    background,
    foreground,
    surface: mix(background, foreground, 0.035),
    accent: config.cursor ?? config.palette[4] ?? mix(background, foreground, 0.7),
    cursor: config.cursor ?? foreground,
    selection: config.selectionBackground ?? mix(background, foreground, 0.2),
    danger: config.palette[1] ?? "#e45f57",
    warning: config.palette[3] ?? "#d6a84f",
    success: config.palette[2] ?? "#7fae8b",
  };
  if (palette) theme.palette = palette;
  if (config.selectionForeground) theme.selectionForeground = config.selectionForeground;
  if (config.splitDividerColor) theme.divider = config.splitDividerColor;
  return theme;
}

function luminance(color: string): number {
  const value = Number.parseInt(color.slice(1), 16);
  const channel = (shift: number): number => ((value >> shift) & 0xff) / 255;
  return 0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0);
}

// ── Display preferences ──────────────────────────────────────────────

/**
 * `base` with everything the Ghostty config specifies: font, cell size,
 * padding, unfocused dimming, Option-as-Alt, and the Ghostty theme. Ghostty's
 * cell height is the font's natural height, so line height becomes 1.
 */
export function ghosttyPrefs(base: DisplayPrefs, config: GhosttyConfig): DisplayPrefs {
  const next: DisplayPrefs = { ...base, ghosttyKeys: true };
  if (config.fontFamily) next.fontFamily = config.fontFamily;
  if (config.fontSize) next.fontSize = Math.round(config.fontSize);
  if (config.fontFamily || config.fontSize || config.cellHeight) next.lineHeight = 1;
  if (config.cellWidth) next.cellWidth = config.cellWidth;
  if (config.cellHeight) next.cellHeight = config.cellHeight;
  if (config.paddingX !== undefined) next.paddingX = config.paddingX + 2;
  if (config.paddingY !== undefined) next.paddingY = config.paddingY + 2;
  if (config.unfocusedSplitOpacity !== undefined)
    next.unfocusedOpacity = config.unfocusedSplitOpacity;
  if (config.optionAsAlt !== undefined) next.optionAsAlt = config.optionAsAlt;
  if (ghosttyTheme(config)) next.themeId = "ghostty";
  return next;
}
