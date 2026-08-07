import assert from "node:assert/strict";
import test from "node:test";

import type { AgentToolResult, ExtensionAPI, Theme } from "@earendil-works/pi-coding-agent";

import { PhuxCli } from "../src/adapter.js";
import type { ProcessResult, RunRequest } from "../src/runner.js";
import { PhuxTargetStore } from "../src/target-store.js";
import {
  MAX_MODEL_BYTES,
  MAX_MODEL_LINES,
  PhuxAskParams,
  PhuxCreateParams,
  PhuxInsertPaneParams,
  PhuxKillParams,
  PhuxLaunchParams,
  PhuxListParams,
  PhuxMovePaneParams,
  PhuxPanesParams,
  PhuxRenderedSnapshotParams,
  PhuxRunParams,
  PhuxSendKeysParams,
  PhuxSignalParams,
  PhuxSnapshotParams,
  PhuxSpawnParams,
  PhuxSwapPaneParams,
  PhuxTagParams,
  PhuxTargetsParams,
  PhuxWaitParams,
  PhuxWatchParams,
  boundedResult,
  registerPhuxTools,
  resolveTarget,
  sanitizeRenderText,
  type PhuxToolDetails,
} from "../src/tools.js";

interface CapturedTool {
  readonly name: string;
  readonly parameters: unknown;
  execute(id: string, params: Record<string, unknown>, signal?: AbortSignal): Promise<AgentToolResult<PhuxToolDetails>>;
  renderCall?: (args: Record<string, unknown>, theme: Theme, context: unknown) => { render(width: number): string[] };
  renderResult?: (result: AgentToolResult<PhuxToolDetails>, options: unknown, theme: Theme, context: unknown) => { render(width: number): string[] };
}

interface ObjectSchema {
  readonly type?: string;
  readonly additionalProperties?: boolean;
  readonly anyOf?: readonly ObjectSchema[];
  readonly required?: readonly string[] | undefined;
  readonly not?: { readonly required?: readonly string[] };
  readonly properties?: Record<string, {
    readonly type?: string;
    readonly minItems?: number;
    readonly minimum?: number;
    readonly exclusiveMinimum?: number;
    readonly exclusiveMaximum?: number;
  }>;
}

type AgentFixture = ReturnType<typeof agentPane>;

const completed = (stdout: string, exitCode = 0): ProcessResult => ({
  termination: "completed", exitCode, stdout, stderr: "",
});

const agentPane = (terminal: string, session = "work", window = `window-${terminal.slice(1)}`) => ({
  terminal, session, window,
  agent: { id: "codex", label: "Codex", kind: "codex" },
  state: "working", confidence: 0.9, attention: "normal",
  title: null, cwd: "/repo", sources: [], explanation: "working cue",
});

const renderStyle = {
  bold: false, faint: false, italic: false, underline: false, blink: false,
  inverse: false, invisible: false, strikethrough: false, overline: false,
  fg: { kind: "default" }, bg: { kind: "default" },
};

function fixture(options: {
  readonly runOutput?: string;
  readonly runTruncated?: boolean;
  readonly snapshotScrollback?: readonly string[];
  readonly agentLists?: readonly (readonly AgentFixture[] | Error)[];
  readonly watchEvents?: number;
} = {}): {
  readonly tools: Map<string, CapturedTool>;
  readonly requests: RunRequest[];
  readonly store: PhuxTargetStore;
} {
  const tools = new Map<string, CapturedTool>();
  const requests: RunRequest[] = [];
  const inventories = options.agentLists ?? [[agentPane("@3"), agentPane("@4"), agentPane("@9"), agentPane("@10")]];
  let inventoryIndex = 0;
  const cli = new PhuxCli({
    socket: "/tmp/phux.sock",
    runner: async (request) => {
      requests.push(request);
      switch (request.args[0]) {
        case "new": return completed(JSON.stringify({ session: "fresh", terminal_id: 9 }));
        case "snapshot": {
          if (request.args.includes("--rendered")) {
            const cols = Number(request.args[request.args.indexOf("--cols") + 1]);
            const rows = Number(request.args[request.args.indexOf("--rows") + 1]);
            return completed(JSON.stringify({
              schema_version: 1, cols, rows, cursor: null,
              cells: Array.from({ length: cols * rows }, () => ({ grapheme: "x", style: renderStyle })),
            }));
          }
          return completed(JSON.stringify({
            schema_version: 3, pane: 9, cols: 80, rows: 1, cursor: null,
            lines: ["ready"], scrollback: options.snapshotScrollback ?? [],
          }));
        }
        case "send-keys": return completed("");
        case "run": return completed(JSON.stringify({
          command: "echo ok", exit_code: 0, output: options.runOutput ?? "ok", duration_ms: 4,
          truncated: options.runTruncated ?? false,
        }));
        case "wait": return completed(JSON.stringify({
          schema_version: 3, pane: 9, cols: 80, rows: 1, cursor: null,
          lines: ["done"], scrollback: [],
        }));
        case "ls": return completed(JSON.stringify({
          schema_version: 1, sessions: [{ name: "work", windows: 2, attached: false }],
        }));
        case "agent": {
          const inventory = inventories[Math.min(inventoryIndex, inventories.length - 1)];
          inventoryIndex++;
          if (inventory instanceof Error) throw inventory;
          return completed(JSON.stringify({ schema_version: 1, agents: inventory ?? [] }));
        }
        case "spawn": return completed(JSON.stringify({ terminal_id: 9, satellite: null }));
        case "launch": return completed(JSON.stringify({
          schema_version: 1, terminal_id: 10, integration: "codex", plugin: "agents", argv: ["secret"],
        }));
        case "insert-pane": return completed(JSON.stringify({
          schema_version: 1, operation: "insert-pane", session_id: 1,
          target_terminal_id: 3, new_terminal_id: 4, direction: "vertical", ratio: 0.4,
        }));
        case "move-pane": return completed(JSON.stringify({
          schema_version: 1, operation: "move-pane", session_id: 1,
          source_terminal_id: 4, target_terminal_id: 3, direction: "horizontal", ratio: 0.6,
        }));
        case "swap-pane": return completed(JSON.stringify({
          schema_version: 1, operation: "swap-pane", session_id: 1,
          first_terminal_id: 3, second_terminal_id: 4,
        }));
        case "kill": case "signal": return completed("");
        case "tag": return completed(`${request.args[2] ?? "@3"}\tbuild ci\n`);
        case "ask": return completed(JSON.stringify({
          event: "asked", terminal: request.args[1] ?? "@3", id: "q", question: "Approve?",
          suggestions: [], elapsed_seconds: null,
        }));
        case "watch": return {
          termination: "timed_out", exitCode: null, stderr: "",
          stdout: Array.from({ length: options.watchEvents ?? 2 }, (_, index) =>
            JSON.stringify({ event: "dirty", terminal: `@${String(index + 1)}` })).join("\n"),
        };
        default: throw new Error(`unexpected argv ${JSON.stringify(request.args)}`);
      }
    },
  });
  const store = new PhuxTargetStore({ appendEntry: () => {} }, cli);
  const api = {
    registerTool: (definition: unknown) => {
      const captured = definition as CapturedTool;
      tools.set(captured.name, captured);
    },
  } as unknown as ExtensionAPI;
  registerPhuxTools(api, cli, store);
  return { tools, requests, store };
}

function tool(tools: Map<string, CapturedTool>, name: string): CapturedTool {
  const found = tools.get(name);
  assert.notEqual(found, undefined);
  return found as CapturedTool;
}

const text = (result: AgentToolResult<PhuxToolDetails>): string =>
  result.content[0]?.type === "text" ? result.content[0].text : "";

const theme = {
  fg: (_color: string, value: string) => value,
  bold: (value: string) => value,
} as unknown as Theme;

test("every registered tool schema, including union branches, is strict", () => {
  const assertStrict = (schema: ObjectSchema): void => {
    if (schema.type === "object") assert.equal(schema.additionalProperties, false);
    for (const branch of schema.anyOf ?? []) assertStrict(branch);
  };
  for (const schema of [
    PhuxListParams, PhuxCreateParams, PhuxSnapshotParams, PhuxSendKeysParams,
    PhuxRunParams, PhuxWaitParams, PhuxPanesParams, PhuxSpawnParams,
    PhuxLaunchParams, PhuxInsertPaneParams, PhuxMovePaneParams, PhuxSwapPaneParams,
    PhuxKillParams, PhuxSignalParams, PhuxTagParams,
    PhuxAskParams, PhuxWatchParams, PhuxRenderedSnapshotParams, PhuxTargetsParams,
  ]) assertStrict(schema as ObjectSchema);

  assert.equal((PhuxCreateParams as ObjectSchema).properties?.command?.minItems, 1);
  assert.equal((PhuxSendKeysParams as ObjectSchema).properties?.keys?.minItems, 1);
  assert.equal((PhuxRunParams as ObjectSchema).properties?.command?.type, "string");
  assert.equal((PhuxRunParams as ObjectSchema).properties?.timeout_seconds?.minimum, 0);
  assert.equal((PhuxWaitParams as ObjectSchema).properties?.timeout_seconds?.minimum, 1);
  assert.deepEqual((PhuxWaitParams as ObjectSchema).not?.required, ["until", "idle_ms"]);
});

test("placement and spatial schemas encode conditional requirements and numeric bounds", () => {
  const spawn = PhuxSpawnParams as ObjectSchema;
  const launch = PhuxLaunchParams as ObjectSchema;
  assert.equal(spawn.anyOf?.length, 2);
  assert.equal(spawn.anyOf?.[0]?.properties?.target, undefined);
  assert.equal(spawn.anyOf?.[1]?.properties?.satellite, undefined);
  assert.ok(spawn.anyOf?.[1]?.required?.includes("target"));
  assert.ok(launch.anyOf?.[1]?.required?.includes("target"));

  for (const schema of [
    spawn.anyOf?.[1]?.properties?.ratio,
    launch.anyOf?.[1]?.properties?.ratio,
    (PhuxInsertPaneParams as ObjectSchema).properties?.ratio,
    (PhuxMovePaneParams as ObjectSchema).properties?.ratio,
  ]) {
    assert.equal(schema?.exclusiveMinimum, 0);
    assert.equal(schema?.exclusiveMaximum, 1);
  }
  assert.deepEqual((PhuxSwapPaneParams as ObjectSchema).required, ["first", "second"]);
});

test("list and create execute their registered operations", async () => {
  const { tools, requests, store } = fixture();
  const listed = await tool(tools, "phux_list").execute("list", {});
  const created = await tool(tools, "phux_create").execute("create", {
    name: "fresh", cwd: "/repo", command: ["bash", "-lc", "echo ok"],
  });

  assert.match(text(listed), /sessions=1/);
  assert.deepEqual(requests[1]?.args, [
    "new", "--json", "-s", "fresh", "--cwd", "/repo", "--socket", "/tmp/phux.sock",
    "--", "bash", "-lc", "echo ok",
  ]);
  assert.equal(store.snapshot.selection?.selector, "@9");
  assert.deepEqual(created.details?.selection, store.snapshot.selection);
});

test("snapshot rejects unavailable implicit state while raw selectors remain caller-owned", async () => {
  const { tools, requests, store } = fixture({ agentLists: [new Error("inventory should not run")] });
  assert.throws(() => resolveTarget(undefined, store), /No phux target is selected/);
  await assert.rejects(tool(tools, "phux_snapshot").execute("implicit", {}), /Pass target explicitly/);
  assert.equal(requests.length, 0);

  const result = await tool(tools, "phux_snapshot").execute("raw", { target: "work", scrollback: 20 });
  assert.deepEqual(requests[0]?.args, [
    "snapshot", "--json", "--scrollback", "20", "--socket", "/tmp/phux.sock", "work",
  ]);
  assert.match(text(result), /ready/);
});

test("send, run, and wait execute documented argv with cancellation and timeouts", async () => {
  const { tools, requests } = fixture();
  const controller = new AbortController();
  await tool(tools, "phux_send_keys").execute("send", {
    target: "@9", keys: ["C-c", "Enter"], local_timeout_ms: 500,
  }, controller.signal);
  await tool(tools, "phux_run").execute("run", {
    target: "@9", command: "echo ok", timeout_seconds: 30,
  }, controller.signal);
  const waited = await tool(tools, "phux_wait").execute("wait", {
    target: "@9", until: "done", timeout_seconds: 2,
  }, controller.signal);

  assert.deepEqual(requests.map((request) => request.args), [
    ["send-keys", "--socket", "/tmp/phux.sock", "@9", "C-c", "Enter"],
    ["run", "--json", "--timeout", "30", "--socket", "/tmp/phux.sock", "@9", "echo ok"],
    ["wait", "--json", "--until", "done", "--timeout", "2", "--socket", "/tmp/phux.sock", "@9"],
  ]);
  assert.equal(requests[0]?.timeoutMs, 500);
  assert.equal(requests[0]?.signal, controller.signal);
  assert.equal(waited.details?.outcome, "satisfied");
});

test("wait rejects zero and competing conditions without executing", async () => {
  const { tools, requests } = fixture();
  await assert.rejects(tool(tools, "phux_wait").execute("zero", {
    target: "@9", timeout_seconds: 0,
  }), /positive integer; omit it to wait indefinitely/);
  await assert.rejects(tool(tools, "phux_wait").execute("both", {
    target: "@9", until: "done", idle_ms: 10,
  }), /either until or idle_ms, not both/);
  assert.equal(requests.length, 0);
});

test("panes, spawn, and launch execute and save validated aliases", async () => {
  const { tools, requests, store } = fixture();
  const panes = await tool(tools, "phux_panes").execute("panes", {});
  await tool(tools, "phux_spawn").execute("spawn", { alias: "shell", command: ["bash"] });
  await tool(tools, "phux_launch").execute("launch", { integration: "codex", alias: "agent" });

  assert.equal(panes.details?.count, 4);
  assert.equal(store.named.aliases.shell?.selector, "@9");
  assert.equal(store.named.aliases.agent?.selector, "@10");
  assert.deepEqual(requests.filter((request) => request.args[0] === "spawn")[0]?.args,
    ["spawn", "--json", "--socket", "/tmp/phux.sock", "--", "bash"]);
  assert.deepEqual(requests.filter((request) => request.args[0] === "launch")[0]?.args,
    ["launch", "--json", "--socket", "/tmp/phux.sock", "codex"]);
});

test("placement and spatial tools execute exact bounded CLI actions", async () => {
  const { tools, requests } = fixture();
  const controller = new AbortController();
  await tool(tools, "phux_spawn").execute("placed-spawn", {
    target: "@3", split: "vertical", ratio: 0.4, local_timeout_ms: 700,
  }, controller.signal);
  await tool(tools, "phux_launch").execute("placed-launch", {
    integration: "codex", target: "@3", split: "horizontal", ratio: 0.6,
  });
  await tool(tools, "phux_insert_pane").execute("insert", {
    target: "@3", new_pane: "@4", direction: "vertical", ratio: 0.4,
  });
  await tool(tools, "phux_move_pane").execute("move", {
    source: "@4", target: "@3", direction: "horizontal", ratio: 0.6, local_timeout_ms: 800,
  }, controller.signal);
  await tool(tools, "phux_swap_pane").execute("swap", { first: "@3", second: "@4" });

  assert.deepEqual(requests.map((request) => request.args), [
    ["spawn", "--json", "--target", "@3", "--split", "vertical", "--ratio", "0.4", "--socket", "/tmp/phux.sock"],
    ["launch", "--json", "--target", "@3", "--split", "horizontal", "--ratio", "0.6", "--socket", "/tmp/phux.sock", "codex"],
    ["insert-pane", "--json", "--split", "vertical", "--ratio", "0.4", "--socket", "/tmp/phux.sock", "@3", "@4"],
    ["move-pane", "--json", "--split", "horizontal", "--ratio", "0.6", "--socket", "/tmp/phux.sock", "@4", "@3"],
    ["swap-pane", "--json", "--socket", "/tmp/phux.sock", "@3", "@4"],
  ]);
  assert.equal(requests[0]?.timeoutMs, 700);
  assert.equal(requests[0]?.signal, controller.signal);
  assert.equal(requests[3]?.timeoutMs, 800);
  assert.equal(requests[3]?.signal, controller.signal);
});

test("placement validation rejects satellite and incomplete requests without executing", async () => {
  const { tools, requests } = fixture();
  await assert.rejects(tool(tools, "phux_spawn").execute("satellite", {
    target: "edge/@7", split: "vertical",
  }), /local-only/);
  await assert.rejects(tool(tools, "phux_launch").execute("missing", {
    integration: "codex", ratio: 0.4,
  }), /target is required/);
  assert.equal(requests.length, 0);
});

test("targets execute mutations and group expansion for tag and confirmed kill", async () => {
  const { tools, requests } = fixture();
  await tool(tools, "phux_targets").execute("group", {
    action: "set_group", name: "workers", targets: ["@3", "@4"],
  });
  const targets = await tool(tools, "phux_targets").execute("list-targets", { action: "list" });
  await tool(tools, "phux_tag").execute("tag", {
    action: "add", target: "group:workers", tags: ["build", "ci"],
  });
  const killed = await tool(tools, "phux_kill").execute("kill", {
    target: "group:workers", confirm: true,
  });

  assert.match(text(targets), /group:workers/);
  assert.deepEqual(requests.filter((request) => request.args[0] === "tag").map((request) => request.args[2]), ["@3", "@4"]);
  assert.deepEqual(requests.filter((request) => request.args[0] === "kill").map((request) => request.args[1]), ["@3", "@4"]);
  assert.equal(killed.details?.count, 2);
});

test("kill and destructive signals require explicit strong confirmation", async () => {
  const { tools, requests } = fixture();
  await assert.rejects(tool(tools, "phux_kill").execute("kill-no-confirm", { target: "work" }), /confirm:true/);
  await assert.rejects(tool(tools, "phux_kill").execute("kill-no-target", { confirm: true }), /confirm:true|target/);
  await assert.rejects(tool(tools, "phux_signal").execute("signal-no-confirm", {
    target: "work", signal: "terminate",
  }), /explicit target and confirm:true/);
  await tool(tools, "phux_signal").execute("signal-safe", { target: "@3", signal: "freeze" });
  await tool(tools, "phux_signal").execute("signal-confirmed", {
    target: "work", signal: "kill", confirm: true,
  });

  assert.deepEqual(requests.filter((request) => request.args[0] === "signal").map((request) => request.args), [
    ["signal", "@3", "freeze", "--socket", "/tmp/phux.sock"],
    ["signal", "work", "kill", "--socket", "/tmp/phux.sock"],
  ]);
  assert.equal(requests.some((request) => request.args[0] === "kill"), false);
});

test("ask executes and watch returns bounded event output", async () => {
  const { tools, requests } = fixture({ watchEvents: 150 });
  const asked = await tool(tools, "phux_ask").execute("ask", {
    target: "@3", question: "Approve?", id: "q",
  });
  const watched = await tool(tools, "phux_watch_events").execute("watch", {
    target: "@3", duration_ms: 250, max_events: 100,
  });

  assert.match(text(asked), /Reported ask/);
  assert.equal(watched.details?.count, 100);
  assert.equal(watched.details?.modelOutputTruncated, true);
  assert.ok(Buffer.byteLength(text(watched)) <= MAX_MODEL_BYTES);
  assert.ok(text(watched).split("\n").length <= MAX_MODEL_LINES);
  assert.equal(requests.find((request) => request.args[0] === "watch")?.timeoutMs, 250);
});

test("rendered snapshot executes and bounds the maximum frame", async () => {
  const { tools, requests } = fixture();
  const rendered = await tool(tools, "phux_rendered_snapshot").execute("render", {
    session: "work", cols: 160, rows: 80,
  });

  assert.deepEqual(requests[0]?.args, [
    "snapshot", "--rendered", "--json", "--cols", "160", "--rows", "80",
    "--socket", "/tmp/phux.sock", "work",
  ]);
  assert.equal(rendered.details?.modelOutputTruncated, true);
  assert.ok(Buffer.byteLength(text(rendered)) <= MAX_MODEL_BYTES);
  assert.ok(text(rendered).split("\n").length <= MAX_MODEL_LINES);
});

test("named actions refresh ownership immediately and reject reused ids", async () => {
  const original = agentPane("@3");
  const reused = agentPane("@3", "foreign", "window-99");
  const { tools, requests } = fixture({ agentLists: [[original], [reused]] });
  await tool(tools, "phux_targets").execute("alias", {
    action: "set_alias", name: "build", target: "@3",
  });
  await assert.rejects(tool(tools, "phux_snapshot").execute("stale", {
    target: "alias:build",
  }), /ownership changed/);

  assert.equal(requests.filter((request) => request.args[0] === "agent").length, 2);
  assert.equal(requests.some((request) => request.args[0] === "snapshot"), false);
});

test("spatial aliases refresh ownership immediately and fail before topology mutation", async () => {
  const original = agentPane("@3");
  const reused = agentPane("@3", "foreign", "window-99");
  const { tools, requests } = fixture({ agentLists: [[original, agentPane("@4")], [reused, agentPane("@4")]] });
  await tool(tools, "phux_targets").execute("alias", {
    action: "set_alias", name: "build", target: "@3",
  });
  await assert.rejects(tool(tools, "phux_move_pane").execute("stale-move", {
    source: "alias:build", target: "@4",
  }), /ownership changed/);

  assert.equal(requests.filter((request) => request.args[0] === "agent").length, 2);
  assert.equal(requests.some((request) => request.args[0] === "move-pane"), false);
});

test("named actions fail closed when fresh inventory fails", async () => {
  const { tools, requests } = fixture({ agentLists: [[agentPane("@3")], new Error("server offline")] });
  await tool(tools, "phux_targets").execute("alias", {
    action: "set_alias", name: "build", target: "@3",
  });
  await assert.rejects(tool(tools, "phux_send_keys").execute("offline", {
    target: "alias:build", keys: ["Enter"],
  }), /server offline/);

  assert.equal(requests.filter((request) => request.args[0] === "agent").length, 2);
  assert.equal(requests.some((request) => request.args[0] === "send-keys"), false);
});

test("bounded terminal results expose adapter and phux truncation", async () => {
  const byLines = boundedResult("stable header", Array.from({ length: 500 }, (_, index) => `line-${String(index)}`).join("\n"), true);
  assert.match(byLines.text, /Pi adapter truncated terminal output/);
  assert.match(byLines.text, /phux reported that terminal output was already truncated/);
  assert.ok(byLines.text.split("\n").length <= MAX_MODEL_LINES);
  assert.ok(Buffer.byteLength(byLines.text) <= MAX_MODEL_BYTES);

  const { tools } = fixture({ runOutput: "x".repeat(MAX_MODEL_BYTES * 2), runTruncated: true });
  const result = await tool(tools, "phux_run").execute("truncated-run", { target: "@9", command: "printf lots" });
  assert.match(text(result), /Pi adapter truncated terminal output/);
  assert.match(text(result), /phux reported that terminal output was already truncated/);
});

test("custom renderers sanitize dynamic fields and show compact summaries", async () => {
  const { tools } = fixture();
  const create = tool(tools, "phux_create");
  const created = await create.execute("create", { name: "fresh" });
  assert.match(create.renderResult?.(created, {}, theme, {}).render(80).join("\n") ?? "", /created fresh:window-0 @9/);

  const malicious = "bad\x1b[31mRED\x1b[0m\nNEXT\x9b31mC1\x07";
  const run = tool(tools, "phux_run");
  const call = run.renderCall?.({ command: malicious, target: "@9\rspoof" }, theme, {}).render(200).join("\n") ?? "";
  const move = tool(tools, "phux_move_pane");
  const spatialCall = move.renderCall?.({ source: malicious, target: "@9\rspoof" }, theme, {}).render(200).join("\n") ?? "";
  assert.equal(sanitizeRenderText(malicious), "badRED NEXTC1 ");
  assert.doesNotMatch(`${call}${spatialCall}`, /\x1b|\x9b|\x07|\r|\[31m/);
});
