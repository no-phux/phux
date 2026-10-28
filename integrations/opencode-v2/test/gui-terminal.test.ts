import { expect, test } from "bun:test";

import {
  batchKeys,
  keyAction,
  lineText,
  literalKeys,
  parseFormatterHtml,
  reuseLines,
  sanitizeStyle,
  screenText,
  shortPath,
  withCaret,
  type KeyInput,
} from "../src/gui/terminal.js";

const FORMATTER = '<div style="font-family: monospace; white-space: pre;"><div style="display: inline;font-weight: bold;">B</div> '
  + '<div style="display: inline;color: var(--vt-palette-1);">red</div>\n'
  + '<div style="display: inline;filter: invert(100%);">R</div> &lt;b&gt;&amp;amp;&#9474;</div>';

test("the phux Formatter's html becomes styled lines of plain text", () => {
  const lines = parseFormatterHtml(FORMATTER);
  expect(lines.map(lineText)).toEqual(["B red", "R <b>&amp;│"]);
  expect(lines[0]!.segments).toEqual([
    { text: "B", style: { "font-weight": "bold" } },
    { text: " " },
    { text: "red", style: { color: "var(--vt-palette-1)" } },
  ]);
  expect(lines[1]!.segments[0]).toEqual({ text: "R", style: { filter: "invert(100%)" } });
});

test("markup never reaches the page: tags, attributes, and unsafe styles are dropped", () => {
  const hostile = '<script>alert(1)</script><img src=x onerror="alert(2)"><div style="background-image: url(javascript:alert(3)); color: expression(alert(4)); position: fixed; color: rgb(1, 2, 3)">ok</div><a href="javascript:x">link</a><';
  const lines = parseFormatterHtml(hostile);
  expect(lines.map(lineText)).toEqual(["alert(1)oklink<"]);
  const styled = lines[0]!.segments.find((segment) => segment.text === "ok");
  expect(styled?.style).toEqual({ color: "rgb(1, 2, 3)" });
  expect(sanitizeStyle("color: red; opacity: 0.5; visibility: hidden; text-decoration-line: underline line-through")).toEqual({
    opacity: "0.5",
    visibility: "hidden",
    "text-decoration-line": "underline line-through",
  });
  expect(sanitizeStyle("color: var(--evil); background-color: #12345")).toEqual({ "background-color": "#12345" });
});

test("entities decode, and invalid code points become the replacement character", () => {
  expect(lineText(parseFormatterHtml("&#x1F600;&#0;&#xD800;&nbsp;&unknown;")[0]!)).toBe("😀\ufffd\ufffd\u00a0&unknown;");
});

test("trimmed trailing rows are restored and the caret lands in the viewport", () => {
  // Two history rows, a three-row viewport whose last row the Formatter trimmed.
  const lines = parseFormatterHtml("h1\nh2\n$ ls\nfile");
  const shown = withCaret(lines, 2, 3, { x: 2, y: 2 });
  expect(shown.map(lineText)).toEqual(["h1", "h2", "$ ls", "file", "   "]);
  expect(shown[4]!.segments.at(-1)).toEqual({ text: " ", caret: true });

  const inside = withCaret(parseFormatterHtml("abc"), 0, 1, { x: 1, y: 0 });
  expect(inside[0]!.segments).toEqual([{ text: "a" }, { text: "b", caret: true }, { text: "c" }]);
});

test("unchanged lines keep their identity across renders", () => {
  const first = parseFormatterHtml("same\nold");
  const second = reuseLines(first, parseFormatterHtml("same\nnew"));
  expect(second[0]).toBe(first[0]);
  expect(second[1]).not.toBe(first[1]);
});

test("quoting keeps the last non-blank rows", () => {
  const lines = parseFormatterHtml("a\nb  \nc\n\n");
  expect(screenText(withCaret(lines, 0, 6, null), 2)).toBe("b\nc");
});

const key = (key: string, extra: Partial<KeyInput> = {}): KeyInput => ({
  key,
  code: extra.code ?? "",
  ctrlKey: false,
  altKey: false,
  metaKey: false,
  shiftKey: false,
  ...extra,
});

test("keystrokes translate to phux key specs, and app shortcuts stay with the app", () => {
  expect(keyAction(key("a", { code: "KeyA" }))).toEqual({ kind: "text", text: "a" });
  expect(keyAction(key("Enter"))).toEqual({ kind: "key", spec: "Enter" });
  expect(keyAction(key("Backspace"))).toEqual({ kind: "key", spec: "BSpace" });
  expect(keyAction(key("ArrowUp"))).toEqual({ kind: "key", spec: "Up" });
  expect(keyAction(key("c", { code: "KeyC", ctrlKey: true }))).toEqual({ kind: "key", spec: "C-c" });
  expect(keyAction(key("[", { code: "BracketLeft", ctrlKey: true }))).toEqual({ kind: "key", spec: "Escape" });
  expect(keyAction(key("å", { code: "KeyA", altKey: true }))).toEqual({ kind: "key", spec: "M-a" });
  expect(keyAction(key("Enter", { altKey: true }))).toEqual({ kind: "key", spec: "\u001b\r" });
  expect(keyAction(key("Tab", { shiftKey: true }))).toEqual({ kind: "key", spec: "\u001b[Z" });
  expect(keyAction(key("Delete"))).toEqual({ kind: "key", spec: "\u001b[3~" });
  expect(keyAction(key("v", { code: "KeyV", metaKey: true }))).toBeUndefined();
  expect(keyAction(key("Shift"))).toBeUndefined();
});

test("typed text can never be read as a key name, and raw sequences are never pasted", () => {
  expect(literalKeys("hello")).toEqual(["hello"]);
  expect(literalKeys("Up")).toEqual(["U", "p"]);
  expect(literalKeys("c-x")).toEqual(["c", "-", "x"]);
  expect(literalKeys("m-anything")).toHaveLength(10);
  expect(batchKeys([
    { kind: "text", text: "l" },
    { kind: "text", text: "s" },
    { kind: "key", spec: "Enter" },
    { kind: "key", spec: "\u001b[3~" },
    { kind: "text", text: "x" },
  ])).toEqual([["ls", "Enter", "\u001b[3~"], ["x"]]);
});

test("paths under a home directory shorten to ~", () => {
  expect(shortPath("/Users/ada")).toBe("~");
  expect(shortPath("/home/ada/src/app")).toBe("~/src/app");
  expect(shortPath("/srv/a/b/c/d")).toBe("…/b/c/d");
  expect(shortPath(null)).toBe("");
});

test("Control-Tab stays with the app", () => {
  expect(keyAction(key("Tab", { ctrlKey: true }))).toBeUndefined();
});
