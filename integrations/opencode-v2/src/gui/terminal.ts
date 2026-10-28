/**
 * Pure helpers for showing and driving a phux pane in the GUI, kept free of
 * Solid and the host so they are unit-testable.
 */

export type TextStyle = Readonly<Record<string, string>>;

export interface Segment {
  readonly text: string;
  readonly style?: TextStyle;
  readonly caret?: true;
}

export interface Line {
  /** Content identity: equal keys render identically, so a view can keep their DOM. */
  readonly key: string;
  readonly segments: readonly Segment[];
}

const COLOR = /^(?:rgb\(\s*\d{1,3}\s*,\s*\d{1,3}\s*,\s*\d{1,3}\s*\)|#[0-9a-f]{3,8}|var\(--vt-palette-\d{1,3}\))$/i;
const ALLOWED: Readonly<Record<string, (value: string) => boolean>> = {
  color: (value) => COLOR.test(value),
  "background-color": (value) => COLOR.test(value),
  "font-weight": (value) => /^(?:bold|normal|[1-9]00)$/.test(value),
  "font-style": (value) => /^(?:italic|normal)$/.test(value),
  "text-decoration-line": (value) => /^(?:none|(?:underline|overline|line-through)(?:\s+(?:underline|overline|line-through))*)$/.test(value),
  "text-decoration-style": (value) => /^(?:solid|double|dotted|dashed|wavy)$/.test(value),
  opacity: (value) => /^(?:0|1|0?\.\d+)$/.test(value),
  visibility: (value) => value === "hidden",
  filter: (value) => value === "invert(100%)",
};

/**
 * Keep only the presentational declarations the phux Formatter emits, with
 * values that cannot reach a URL, script, or layout outside the text.
 */
export function sanitizeStyle(declarations: string): TextStyle | undefined {
  const style: Record<string, string> = {};
  for (const declaration of declarations.split(";")) {
    const colon = declaration.indexOf(":");
    if (colon < 0) continue;
    const property = declaration.slice(0, colon).trim().toLowerCase();
    const value = declaration.slice(colon + 1).trim();
    const allowed = ALLOWED[property];
    if (allowed !== undefined && allowed(value)) style[property] = value;
  }
  return Object.keys(style).length === 0 ? undefined : style;
}

const NAMED_ENTITIES: Readonly<Record<string, string>> = {
  amp: "&", lt: "<", gt: ">", quot: "\"", apos: "'", nbsp: "\u00a0",
};

export function decodeEntities(text: string): string {
  return text.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (match, entity: string) => {
    if (entity[0] === "#") {
      const code = entity[1] === "x" || entity[1] === "X" ? Number.parseInt(entity.slice(2), 16) : Number.parseInt(entity.slice(1), 10);
      return Number.isInteger(code) && code >= 0 && code <= 0x10ffff && (code < 0xd800 || code > 0xdfff) && code !== 0
        ? String.fromCodePoint(code)
        : "\ufffd";
    }
    return NAMED_ENTITIES[entity.toLowerCase()] ?? match;
  });
}

const TOKEN = /<(\/?)([a-zA-Z][a-zA-Z0-9]*)((?:[^>"']|"[^"]*"|'[^']*')*)>|([^<]+)|(<)/g;
const STYLE_ATTRIBUTE = /\bstyle\s*=\s*(?:"([^"]*)"|'([^']*)')/i;

/**
 * Parse the phux server's html rendering into lines of styled text. The
 * markup is never assigned as HTML: tags only scope styles, every other tag
 * and attribute is dropped, and text is decoded into plain strings.
 */
export function parseFormatterHtml(html: string): Line[] {
  const lines: Segment[][] = [[]];
  const styles: (TextStyle | undefined)[] = [undefined];
  const push = (text: string): void => {
    const style = styles[styles.length - 1];
    const parts = text.split("\n");
    parts.forEach((part, index) => {
      if (index > 0) lines.push([]);
      if (part.length > 0) lines[lines.length - 1]!.push(style === undefined ? { text: part } : { text: part, style });
    });
  };
  for (const match of html.matchAll(TOKEN)) {
    const [, closing, tag, attributes, text, stray] = match;
    if (text !== undefined) {
      push(decodeEntities(text));
    } else if (stray !== undefined) {
      push("<");
    } else if (tag !== undefined) {
      const name = tag.toLowerCase();
      if (name === "br") {
        push("\n");
      } else if (closing === "/") {
        if (styles.length > 1) styles.pop();
      } else if (!attributes?.trimEnd().endsWith("/")) {
        const declared = STYLE_ATTRIBUTE.exec(attributes ?? "");
        const own = declared === null ? undefined : sanitizeStyle(decodeEntities(declared[1] ?? declared[2] ?? ""));
        const parent = styles[styles.length - 1];
        styles.push(own === undefined ? parent : { ...parent, ...own });
      }
    }
  }
  // The Formatter ends the last row with a newline; it is not another row.
  if (lines.length > 1 && lines[lines.length - 1]!.length === 0) lines.pop();
  return lines.map(toLine);
}

const BLANK: Line = { key: "[]", segments: [] };

export function plainLines(rows: readonly string[]): Line[] {
  return rows.map((row) => toLine(row.length === 0 ? [] : [{ text: row }]));
}

function toLine(segments: readonly Segment[]): Line {
  return { key: JSON.stringify(segments), segments: merge(segments) };
}

function merge(segments: readonly Segment[]): Segment[] {
  const merged: Segment[] = [];
  for (const segment of segments) {
    const last = merged[merged.length - 1];
    if (last !== undefined && last.caret === undefined && segment.caret === undefined && sameStyle(last.style, segment.style)) {
      merged[merged.length - 1] = { ...last, text: last.text + segment.text };
    } else {
      merged.push(segment);
    }
  }
  return merged;
}

function sameStyle(left: TextStyle | undefined, right: TextStyle | undefined): boolean {
  if (left === right) return true;
  if (left === undefined || right === undefined) return false;
  const keys = Object.keys(left);
  return keys.length === Object.keys(right).length && keys.every((key) => left[key] === right[key]);
}

/**
 * Complete the viewport and mark the cursor cell. The phux Formatter trims
 * trailing blank rows, so rows are padded back to history plus viewport; the
 * caret is in viewport coordinates. Columns count code points, which is exact
 * except beside wide characters.
 */
export function withCaret(
  input: readonly Line[],
  historyRows: number,
  rows: number,
  caret: { readonly x: number; readonly y: number } | null | undefined,
): Line[] {
  const lines = [...input];
  while (rows > 0 && lines.length < historyRows + rows) lines.push(BLANK);
  if (caret === null || caret === undefined || rows <= 0) return lines;
  const index = Math.min(historyRows, Math.max(0, lines.length - rows)) + caret.y;
  const line = lines[index];
  if (line === undefined) return lines;
  const segments: Segment[] = [];
  let column = 0;
  let placed = false;
  for (const segment of line.segments) {
    const chars = [...segment.text];
    if (placed || caret.x >= column + chars.length) {
      segments.push(segment);
      column += chars.length;
      continue;
    }
    const at = caret.x - column;
    const style = segment.style === undefined ? {} : { style: segment.style };
    if (at > 0) segments.push({ text: chars.slice(0, at).join(""), ...style });
    segments.push({ text: chars[at]!, ...style, caret: true });
    if (at + 1 < chars.length) segments.push({ text: chars.slice(at + 1).join(""), ...style });
    column += chars.length;
    placed = true;
  }
  if (!placed) {
    if (caret.x > column) segments.push({ text: " ".repeat(caret.x - column) });
    segments.push({ text: " ", caret: true });
  }
  lines[index] = { key: `${line.key}\u0000${caret.x}`, segments };
  return lines;
}

/** Reuse line objects whose content did not change, so keyed rendering keeps their DOM. */
export function reuseLines(previous: readonly Line[], next: readonly Line[]): Line[] {
  const known = new Map<string, Line>();
  for (const line of previous) known.set(line.key, line);
  return next.map((line) => known.get(line.key) ?? line);
}

export function lineText(line: Line): string {
  return line.segments.map((segment) => segment.text).join("");
}

/** The last non-blank rows of a screen, for quoting. */
export function screenText(lines: readonly Line[], limit: number): string {
  const rows = lines.map((line) => lineText(line).trimEnd());
  while (rows.length > 0 && rows[rows.length - 1] === "") rows.pop();
  return rows.slice(-limit).join("\n");
}

const NAMED_KEYS = new Set(["enter", "return", "tab", "escape", "esc", "space", "bspace", "backspace", "up", "down", "right", "left", "home", "end"]);

/** True when phux send-keys would read this argument as a key name instead of text. */
export function isKeySpec(argument: string): boolean {
  const lower = argument.toLowerCase();
  if (NAMED_KEYS.has(lower)) return true;
  if (lower.startsWith("c-") && [...argument.slice(2)].length === 1) return true;
  return lower.startsWith("m-");
}

/** Literal text as send-keys arguments that can never be mistaken for key names. */
export function literalKeys(text: string): string[] {
  if (text.length === 0) return [];
  return isKeySpec(text) ? [...text] : [text];
}

export interface KeyInput {
  readonly key: string;
  readonly code: string;
  readonly ctrlKey: boolean;
  readonly altKey: boolean;
  readonly metaKey: boolean;
  readonly shiftKey: boolean;
}

const SPECIAL: Readonly<Record<string, string>> = {
  Enter: "Enter",
  Tab: "Tab",
  Escape: "Escape",
  Backspace: "BSpace",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  Home: "Home",
  End: "End",
  // Literal escape sequences: phux parses them exactly like typed bytes.
  Delete: "\u001b[3~",
  Insert: "\u001b[2~",
  PageUp: "\u001b[5~",
  PageDown: "\u001b[6~",
  F1: "\u001bOP",
  F2: "\u001bOQ",
  F3: "\u001bOR",
  F4: "\u001bOS",
  F5: "\u001b[15~",
  F6: "\u001b[17~",
  F7: "\u001b[18~",
  F8: "\u001b[19~",
  F9: "\u001b[20~",
  F10: "\u001b[21~",
  F11: "\u001b[23~",
  F12: "\u001b[24~",
};

export type KeyAction =
  | { readonly kind: "key"; readonly spec: string }
  | { readonly kind: "text"; readonly text: string };

/**
 * Translate a keydown into what the pane should receive, or undefined to
 * leave the event to the app (Command shortcuts, bare modifiers, IME).
 */
export function keyAction(event: KeyInput, platform: "mac" | "other" = "other"): KeyAction | undefined {
  // Command chords and Control-Tab belong to the app: tab switching must never be trapped.
  if (event.metaKey || (event.ctrlKey && event.key === "Tab")) return undefined;
  // Paste shortcuts reach the paste event, which sends the clipboard as one bracketed paste.
  if (event.ctrlKey && !event.altKey && event.key.toLowerCase() === "v" && (event.shiftKey || platform !== "mac")) return undefined;
  if (event.key === "Tab" && event.shiftKey && !event.ctrlKey && !event.altKey) return { kind: "key", spec: "\u001b[Z" };
  const special = SPECIAL[event.key];
  if (special !== undefined) {
    const bytes = keyBytes(special);
    if (event.altKey && !event.ctrlKey && bytes !== undefined) return { kind: "key", spec: `\u001b${bytes}` };
    return { kind: "key", spec: special };
  }
  const single = [...event.key].length === 1 ? event.key : undefined;
  if (event.ctrlKey && !event.altKey) return controlKey(single !== undefined && /^[ -~]$/.test(single) ? single : codeCharacter(event.code, event.shiftKey));
  if (event.altKey && !event.ctrlKey) {
    // A layout's Option character (such as @ or € on many macOS layouts) is
    // text; Alt that leaves the key unchanged, as on Linux, means Meta.
    const base = codeCharacter(event.code, event.shiftKey);
    if (single !== undefined && base !== undefined && single.toLowerCase() !== base.toLowerCase()) return { kind: "text", text: single };
    const key = single ?? base;
    return key === undefined ? undefined : { kind: "key", spec: `M-${key}` };
  }
  return single === undefined ? undefined : { kind: "text", text: single };
}

function controlKey(base: string | undefined): KeyAction | undefined {
  if (base === undefined) return undefined;
  if (base === " " || base === "@" || base === "2") return { kind: "key", spec: "C-@" };
  if (base === "[") return { kind: "key", spec: "Escape" };
  if (/^[a-z\\\]^_]$/i.test(base)) return { kind: "key", spec: `C-${base.toLowerCase()}` };
  return undefined;
}

const KEY_BYTES: Readonly<Record<string, string>> = { Enter: "\r", Tab: "\t", Escape: "\u001b", BSpace: "\u007f" };

function keyBytes(name: string): string | undefined {
  return KEY_BYTES[name];
}

function codeCharacter(code: string, shift: boolean): string | undefined {
  if (/^Key[A-Z]$/.test(code)) {
    const letter = code.slice(3);
    return shift ? letter : letter.toLowerCase();
  }
  if (/^Digit\d$/.test(code)) return code.slice(5);
  if (code === "Space") return " ";
  if (code === "BracketLeft") return "[";
  if (code === "BracketRight") return "]";
  if (code === "Backslash") return "\\";
  if (code === "Minus") return shift ? "_" : "-";
  return undefined;
}

/**
 * Batch keystrokes into send-keys calls: adjacent text joins into one literal
 * run and named keys keep their place. phux pastes a literal run that
 * precedes Enter, so a raw escape sequence always ends its call: it must be
 * typed, never pasted.
 */
export function batchKeys(actions: readonly KeyAction[]): string[][] {
  const calls: string[][] = [];
  let specs: string[] = [];
  let text = "";
  const flushText = (): void => {
    specs.push(...literalKeys(text));
    text = "";
  };
  for (const action of actions) {
    if (action.kind === "text") {
      text += action.text;
      continue;
    }
    flushText();
    specs.push(action.spec);
    if (action.spec.startsWith("\u001b")) {
      calls.push(specs);
      specs = [];
    }
  }
  flushText();
  if (specs.length > 0) calls.push(specs);
  return calls;
}

const HOME = /^(?:\/Users\/[^/]+|\/home\/[^/]+|\/root)(?=\/|$)/;

/** Shorten a path under a home directory to `~` and keep its last segments. */
export function shortPath(path: string | null): string {
  if (path === null) return "";
  const relative = path.replace(HOME, "~");
  const parts = relative.split("/");
  return parts.length <= 4 ? relative : `…/${parts.slice(-3).join("/")}`;
}
