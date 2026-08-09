import assert from "node:assert/strict";
import test from "node:test";

import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

import { PhuxCli } from "../src/adapter.js";
import {
  formatAttachHandoff,
  formatDetailedStatus,
  registerPhuxExtension,
} from "../src/extension.js";
import type { PhuxTargetSnapshot } from "../src/target-store.js";

const selected: PhuxTargetSnapshot = {
  selection: {
    version: 1,
    selector: "@3",
    session: "work space;$(bad)",
    window: "window-0",
    display: "work space:window-0 @3 - Codex",
  },
  availability: "stale",
  reason: "pane @3 is no longer present",
};

test("attach handoff is an argv presentation only and retains pane navigation", () => {
  const message = formatAttachHandoff(selected);

  assert.match(message, /\["phux","attach","work space;\$\(bad\)"\]/);
  assert.match(message, /navigate in phux to pane @3/);
  assert.match(message, /does not execute attach/);
  assert.match(message, /without fallback/);
  assert.doesNotMatch(message, /token|cwd|title/i);
});

test("detailed status exposes stale state instead of choosing another pane", () => {
  assert.equal(
    formatDetailedStatus(selected),
    "phux: work space:window-0 @3 - Codex (stale)\npane @3 is no longer present",
  );
});

test("injects phux context after the user message and forces a checkpoint after compaction", async () => {
  type Handler = (event: unknown, ctx: ExtensionContext) => unknown;
  const handlers = new Map<string, Handler[]>();
  const sent: Array<{ content?: string; display?: boolean }> = [];
  const api = {
    appendEntry: () => {},
    sendMessage: (message: { content?: string; display?: boolean }) => { sent.push(message); },
    on: (name: string, handler: Handler) => {
      const registered = handlers.get(name) ?? [];
      registered.push(handler);
      handlers.set(name, registered);
    },
    registerTool: () => {},
    registerCommand: () => {},
  } as unknown as ExtensionAPI;
  const cli = new PhuxCli({
    runner: async () => ({
      termination: "completed",
      exitCode: 0,
      stderr: "",
      stdout: JSON.stringify({
        schema_version: 1,
        agents: [{
          terminal: "@65",
          session: "phux",
          window: "window-0",
          agent: { id: "pi", label: "Pi", kind: "declared" },
          state: "working",
          confidence: 1,
          attention: "normal",
          title: "not model context",
          cwd: "/repo",
          sources: [],
          explanation: "not model context",
        }],
      }),
    }),
  });
  registerPhuxExtension(api, { cli, env: { PHUX_TERMINAL_ID: "65" } });
  const ctx = {
    signal: undefined,
    sessionManager: { getSessionId: () => "pi-session" },
  } as unknown as ExtensionContext;
  const before = handlers.get("before_agent_start")?.[0];
  const compact = handlers.get("session_compact")?.[0];
  assert.notEqual(before, undefined);
  assert.notEqual(compact, undefined);

  const first = await before?.({}, ctx) as { message?: { content?: string; display?: boolean } } | undefined;
  assert.match(first?.message?.content ?? "", /kind="checkpoint" seq="1"/);
  assert.match(first?.message?.content ?? "", /"self":"@65"/);
  assert.equal(first?.message?.display, false);
  assert.equal(await before?.({}, ctx), undefined, "unchanged state emits no custom message");

  await compact?.({}, ctx);
  assert.equal(sent.length, 1);
  assert.match(sent[0]?.content ?? "", /kind="checkpoint" seq="2"/);
  assert.equal(sent[0]?.display, false);
  assert.equal(await before?.({}, ctx), undefined, "the persisted compaction checkpoint is already current");
});

test("registers Pi-native commands and tolerates custom UI being unavailable", async () => {
  const commands = new Map<string, (args: string, ctx: ExtensionContext) => Promise<void>>();
  const events: string[] = [];
  const tools: string[] = [];
  let appended = 0;
  const api = {
    appendEntry: () => { appended++; },
    on: (name: string) => { events.push(name); },
    registerTool: (tool: { name: string }) => { tools.push(tool.name); },
    registerCommand: (name: string, options: { handler: (args: string, ctx: ExtensionContext) => Promise<void> }) => {
      commands.set(name, options.handler);
    },
  } as unknown as ExtensionAPI;
  const cli = new PhuxCli({
    runner: async () => ({
      termination: "completed",
      exitCode: 0,
      stderr: "",
      stdout: JSON.stringify({
        schema_version: 1,
        agents: [{
          terminal: "@3",
          session: "work",
          window: "window-0",
          agent: { id: "codex", label: "Codex", kind: "codex" },
          state: "working",
          confidence: 0.9,
          attention: "normal",
          title: null,
          cwd: "/repo",
          sources: [],
          explanation: "working cue",
        }],
      }),
    }),
  });
  registerPhuxExtension(api, { cli });

  assert.deepEqual([...commands.keys()], ["phux", "phux-status", "phux-attach"]);
  assert.deepEqual(tools, [
    "phux_list", "phux_create", "phux_snapshot", "phux_send_keys", "phux_run", "phux_wait",
    "phux_panes", "phux_spawn", "phux_launch", "phux_insert_pane", "phux_move_pane", "phux_swap_pane",
    "phux_kill", "phux_signal",
    "phux_tag", "phux_ask", "phux_watch_events",
    "phux_rendered_snapshot", "phux_targets",
  ]);
  // The lifecycle reporter subscribes to session boundaries only. It once also
  // took `agent_start` / `agent_settled` to report a state, which stood the
  // server's `rules/pi.toml` detector down on every pane running this
  // extension (phux-w7z2.38).
  assert.deepEqual(events, [
    "session_start", "session_tree", "before_agent_start", "session_compact",
    "session_start", "session_shutdown",
  ]);

  let customCalls = 0;
  const ctx = {
    hasUI: true,
    signal: undefined,
    ui: {
      custom: async () => { customCalls++; return undefined; },
      setStatus: () => {},
      notify: () => {},
    },
  } as unknown as ExtensionContext;
  await commands.get("phux")?.("", ctx);
  assert.equal(customCalls, 1);
  assert.equal(appended, 0);
});
