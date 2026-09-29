/** Chord normalization shared by the window key handler and the command registry. */
export interface KeyModifiers {
  cmd: boolean;
  alt: boolean;
  ctrl: boolean;
  shift: boolean;
}

export interface KeyLike {
  key?: string;
  isHeld?: boolean;
  modifiers?: KeyModifiers;
}

/** Shifted punctuation GPUI may report as the produced character. */
const UNSHIFTED: Record<string, string> = {
  "}": "]",
  "{": "[",
  "+": "=",
  _: "-",
  "|": "\\",
  "?": "/",
  "<": ",",
  ">": ".",
};

/**
 * `cmd+shift+p` style chord for an application shortcut, or "" for keys the
 * app does not arbitrate (anything without Command, and key repeats).
 */
export function chordOf(event: KeyLike): string {
  const modifiers = event.modifiers;
  const raw = event.key;
  if (!modifiers?.cmd || !raw || event.isHeld) return "";
  const lowered = raw.toLowerCase();
  const key = UNSHIFTED[lowered] ?? lowered;
  const shift = modifiers.shift || key !== lowered || lowered !== raw;
  return [
    "cmd",
    modifiers.ctrl ? "ctrl" : "",
    modifiers.alt ? "alt" : "",
    shift ? "shift" : "",
    key,
  ]
    .filter(Boolean)
    .join("+");
}

/**
 * Any key as a chord, for lookup against the effective keymap: Ghostty binds
 * non-Command chords such as `ctrl+tab`, `shift+enter` or a bare `f12`. Only
 * an unmodified printable key is typing and never a chord.
 */
export function keyChord(event: KeyLike): string {
  const modifiers = event.modifiers;
  const raw = event.key;
  if (!raw || event.isHeld) return "";
  if (modifiers?.cmd) return chordOf(event);
  const lowered = raw.toLowerCase();
  const key = UNSHIFTED[lowered] ?? lowered;
  const shift = !!modifiers?.shift || key !== lowered || lowered !== raw;
  const ctrl = !!modifiers?.ctrl;
  const alt = !!modifiers?.alt;
  if (!ctrl && !alt && !shift && key.length === 1) return "";
  return [ctrl ? "ctrl" : "", alt ? "alt" : "", shift ? "shift" : "", key]
    .filter(Boolean)
    .join("+");
}

/** Plain-key name for overlay navigation (palette, find bar), modifiers ignored. */
export function plainKey(event: KeyLike): string {
  return (event.key ?? "").toLowerCase();
}

const GLYPHS: Record<string, string> = {
  cmd: "⌘",
  ctrl: "⌃",
  alt: "⌥",
  shift: "⇧",
  enter: "↩",
  up: "↑",
  down: "↓",
  left: "←",
  right: "→",
  escape: "esc",
  backspace: "⌫",
};

/** Human rendering in macOS modifier order: ⌃⌥⇧⌘ then the key. */
export function displayChord(chord: string): string {
  const parts = chord.split("+");
  const key = parts.at(-1) ?? "";
  const order = ["ctrl", "alt", "shift", "cmd"];
  const mods = order.filter((mod) => parts.includes(mod)).map((mod) => GLYPHS[mod] ?? mod);
  const label = GLYPHS[key] ?? (key.length === 1 ? key.toUpperCase() : key);
  return `${mods.join("")}${label}`;
}
