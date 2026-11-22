import assert from "node:assert/strict";
import test from "node:test";

import type { Theme } from "@earendil-works/pi-coding-agent";
import { visibleWidth } from "@earendil-works/pi-tui";

import { PhuxTargetPicker } from "../src/components.js";
import type { AgentPane } from "../src/schemas.js";

const theme = {
  fg: (_color: string, text: string) => text,
  bold: (text: string) => text,
} as unknown as Theme;

const pane: AgentPane = {
  terminal: "satellite-host/@123",
  session: "a-very-long-session-name",
  window: "window-with-a-long-name",
  agent: { id: "codex", label: "A very long agent label", kind: "codex" },
  state: "blocked",
  confidence: 0.95,
  attention: "high",
  title: null,
  cwd: null,
  sources: [],
  explanation: "blocked",
};

test("target picker respects narrow widths", () => {
  const picker = new PhuxTargetPicker([pane], theme, () => {}, () => {});

  const lines = picker.render(12);

  assert.ok(lines.length > 0);
  assert.ok(lines.every((line) => visibleWidth(line) <= 12));
});

test("target picker handles zero-width renders", () => {
  const picker = new PhuxTargetPicker([pane], theme, () => {}, () => {});
  assert.deepEqual(picker.render(0), []);
});
