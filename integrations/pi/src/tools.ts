import { Type } from "@earendil-works/pi-ai";
import {
  truncateTail,
  type AgentToolResult,
  type ExtensionAPI,
  type Theme,
} from "@earendil-works/pi-coding-agent";
import { Text } from "@earendil-works/pi-tui";

import { PhuxCli } from "./adapter.js";
import type { ScreenState } from "./schemas.js";
import { PhuxTargetStore, type PhuxTargetSelection } from "./target-store.js";

const TARGET = Type.Optional(Type.String({ minLength: 1, maxLength: 512, pattern: ".*\\S.*", description: "Explicit CLI selector or alias:name; otherwise use the selected /phux target" }));
const REQUIRED_TARGET = Type.String({ minLength: 1, maxLength: 512, pattern: ".*\\S.*", description: "CLI selector, alias:name, or (where documented) group:name" });
const TARGET_NAME = Type.String({ minLength: 1, maxLength: 64, pattern: "^[A-Za-z][A-Za-z0-9_-]{0,63}$" });
const PLACEMENT_TARGET = Type.String({ minLength: 1, maxLength: 512, pattern: ".*\\S.*", description: "Exact local CLI pane selector or alias:name" });
const SPLIT = Type.Optional(Type.Union([Type.Literal("horizontal"), Type.Literal("vertical")]));
const RATIO = Type.Optional(Type.Number({ exclusiveMinimum: 0, exclusiveMaximum: 1 }));
const LOCAL_TIMEOUT = Type.Optional(Type.Integer({ minimum: 1, maximum: 86_405_000, description: "Local subprocess deadline in milliseconds; defaults to 10s for short calls or operation timeout + 5s" }));
const RUN_TIMEOUT = Type.Optional(Type.Integer({ minimum: 1, maximum: 86_400, description: "Operation timeout in seconds; defaults to 30; unbounded waits are rejected" }));
const WAIT_TIMEOUT = RUN_TIMEOUT;
const TAIL = Type.Optional(Type.Integer({ minimum: 1, maximum: 100_000 }));
const AGENT_UNTIL = Type.Optional(Type.Array(Type.Union([
  Type.Literal("idle"), Type.Literal("working"), Type.Literal("blocked"), Type.Literal("done"),
]), { minItems: 1, maxItems: 4, uniqueItems: true }));
const NONEMPTY_ARGV = Type.Array(Type.String(), { minItems: 1, maxItems: 256 });
const STRICT = { additionalProperties: false } as const;

export const PhuxListParams = Type.Object({ local_timeout_ms: LOCAL_TIMEOUT }, STRICT);
export const PhuxCreateParams = Type.Object({
  name: Type.String({ minLength: 1, pattern: ".*\\S.*", maxLength: 255 }),
  cwd: Type.Optional(Type.String({ minLength: 1, pattern: ".*\\S.*" })),
  command: Type.Optional(NONEMPTY_ARGV),
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxSnapshotParams = Type.Object({
  target: TARGET,
  scrollback: Type.Optional(Type.Integer({ minimum: 0, maximum: 100_000 })),
  cells: Type.Optional(Type.Boolean()),
  tail: TAIL,
  unwrap: Type.Optional(Type.Boolean()),
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxSendKeysParams = Type.Object({
  target: TARGET,
  keys: Type.Array(Type.String({ minLength: 1 }), { minItems: 1, maxItems: 256 }),
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxPasteParams = Type.Object({
  target: TARGET,
  text: Type.String({ minLength: 1, maxLength: 65_536, description: "Literal text, including newlines; pasted as one input event without submitting Enter" }),
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxAgentPromptParams = Type.Object({
  target: TARGET,
  text: Type.String({ minLength: 1, maxLength: 4096, pattern: "^[^\\r\\n]+$", description: "Single-line prompt; CLI enforces a 4096-byte ceiling" }),
  wait: Type.Optional(Type.Boolean({ description: "Wait for the submitted agent turn; defaults to true" })),
  until: AGENT_UNTIL,
  expect_agent: Type.Optional(Type.String({ minLength: 1, maxLength: 255 })),
  expect_kind: Type.Optional(Type.String({ minLength: 1, maxLength: 255 })),
  timeout_seconds: WAIT_TIMEOUT,
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxAgentWaitParams = Type.Object({
  target: TARGET,
  until: AGENT_UNTIL,
  timeout_seconds: WAIT_TIMEOUT,
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxResourceWaitParams = Type.Object({
  target: Type.String({ minLength: 1, maxLength: 512, pattern: ".*\\S.*", description: "Explicit resource identity accepted by phux resource wait (not a pane alias)" }),
  after: Type.Optional(Type.String({ minLength: 1, maxLength: 512 })),
  timeout_seconds: WAIT_TIMEOUT,
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxStatusParams = Type.Object({ local_timeout_ms: LOCAL_TIMEOUT }, STRICT);
export const PhuxRuntimeInfoParams = Type.Object({ local_timeout_ms: LOCAL_TIMEOUT }, STRICT);
export const PhuxRunParams = Type.Object({
  target: TARGET,
  command: Type.String({ minLength: 1, pattern: ".*\\S.*", description: "One shell command line, passed to phux as a single argument" }),
  timeout_seconds: RUN_TIMEOUT,
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxWaitParams = Type.Object({
  target: TARGET,
  until: Type.Optional(Type.String({ minLength: 1 })),
  regex: Type.Optional(Type.String({ minLength: 1 })),
  output_only: Type.Optional(Type.Boolean()),
  tail: TAIL,
  idle_ms: Type.Optional(Type.Integer({ minimum: 0, maximum: 86_400_000 })),
  timeout_seconds: WAIT_TIMEOUT,
  local_timeout_ms: LOCAL_TIMEOUT,
}, { ...STRICT, not: { anyOf: [
  { required: ["until", "idle_ms"] }, { required: ["until", "regex"] }, { required: ["regex", "idle_ms"] },
] } });
export const PhuxPanesParams = Type.Object({}, STRICT);
const SPAWN_COMMON = {
  cwd: Type.Optional(Type.String({ minLength: 1, pattern: ".*\\S.*" })),
  command: Type.Optional(NONEMPTY_ARGV),
  alias: Type.Optional(TARGET_NAME),
  retain_seconds: Type.Optional(Type.Integer({ minimum: 1, maximum: 86_400, description: "Retain the exited pane for inspection for this many seconds" })),
  local_timeout_ms: LOCAL_TIMEOUT,
};
export const PhuxSpawnParams = Type.Union([
  Type.Object({
    ...SPAWN_COMMON,
    satellite: Type.Optional(Type.String({ minLength: 1, maxLength: 255, pattern: ".*\\S.*" })),
  }, STRICT),
  Type.Object({
    ...SPAWN_COMMON,
    target: PLACEMENT_TARGET,
    split: SPLIT,
    ratio: RATIO,
  }, STRICT),
]);
const LAUNCH_COMMON = {
  integration: Type.String({ minLength: 1, maxLength: 255, pattern: ".*\\S.*" }),
  cwd: Type.Optional(Type.String({ minLength: 1, pattern: ".*\\S.*" })),
  extra: Type.Optional(NONEMPTY_ARGV),
  alias: Type.Optional(TARGET_NAME),
  local_timeout_ms: LOCAL_TIMEOUT,
};
export const PhuxLaunchParams = Type.Union([
  Type.Object(LAUNCH_COMMON, STRICT),
  Type.Object({ ...LAUNCH_COMMON, target: PLACEMENT_TARGET, split: SPLIT, ratio: RATIO }, STRICT),
]);
export const PhuxInsertPaneParams = Type.Object({
  target: PLACEMENT_TARGET,
  new_pane: PLACEMENT_TARGET,
  direction: Type.Optional(Type.Union([Type.Literal("horizontal"), Type.Literal("vertical")])),
  ratio: RATIO,
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxMovePaneParams = Type.Object({
  source: PLACEMENT_TARGET,
  target: PLACEMENT_TARGET,
  direction: Type.Optional(Type.Union([Type.Literal("horizontal"), Type.Literal("vertical")])),
  ratio: RATIO,
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxSwapPaneParams = Type.Object({
  first: PLACEMENT_TARGET,
  second: PLACEMENT_TARGET,
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxKillParams = Type.Object({
  target: REQUIRED_TARGET,
  confirm: Type.Literal(true, { description: "Required acknowledgement that the selector or group may destroy multiple panes" }),
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxSignalParams = Type.Union([
  Type.Object({
    target: TARGET,
    signal: Type.Union([Type.Literal("interrupt"), Type.Literal("freeze"), Type.Literal("resume")]),
    local_timeout_ms: LOCAL_TIMEOUT,
  }, STRICT),
  Type.Object({
    target: REQUIRED_TARGET,
    signal: Type.Union([Type.Literal("terminate"), Type.Literal("kill")]),
    confirm: Type.Literal(true, { description: "Required acknowledgement that the selector may affect multiple processes or panes" }),
    local_timeout_ms: LOCAL_TIMEOUT,
  }, STRICT),
]);
export const PhuxTagParams = Type.Object({
  action: Type.Union([Type.Literal("ls"), Type.Literal("add"), Type.Literal("rm")]),
  target: REQUIRED_TARGET,
  tags: Type.Optional(Type.Array(Type.String({ minLength: 1, maxLength: 255, pattern: ".*\\S.*" }), { minItems: 1, maxItems: 64 })),
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxAskParams = Type.Object({
  target: TARGET,
  question: Type.String({ minLength: 1, maxLength: 4096, pattern: ".*\\S.*" }),
  id: Type.Optional(Type.String({ maxLength: 255 })),
  suggestions: Type.Optional(Type.Array(Type.String({ minLength: 1, maxLength: 512 }), { maxItems: 32 })),
  elapsed_seconds: Type.Optional(Type.Integer({ minimum: 0, maximum: 31_536_000 })),
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxWatchParams = Type.Object({
  target: TARGET,
  duration_ms: Type.Integer({ minimum: 50, maximum: 30_000, description: "Bounded event collection window" }),
  max_events: Type.Optional(Type.Integer({ minimum: 1, maximum: 100 })),
}, STRICT);
export const PhuxRenderedSnapshotParams = Type.Object({
  session: Type.Optional(Type.String({ minLength: 1, maxLength: 255, pattern: ".*\\S.*" })),
  cols: Type.Optional(Type.Integer({ minimum: 20, maximum: 160 })),
  rows: Type.Optional(Type.Integer({ minimum: 5, maximum: 80 })),
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxTargetsParams = Type.Union([
  Type.Object({ action: Type.Literal("list") }, STRICT),
  Type.Object({ action: Type.Literal("set_alias"), name: TARGET_NAME, target: REQUIRED_TARGET }, STRICT),
  Type.Object({ action: Type.Literal("remove_alias"), name: TARGET_NAME }, STRICT),
  Type.Object({ action: Type.Literal("set_group"), name: TARGET_NAME, targets: Type.Array(REQUIRED_TARGET, { minItems: 1, maxItems: 64 }) }, STRICT),
  Type.Object({ action: Type.Literal("remove_group"), name: TARGET_NAME }, STRICT),
]);

export const MAX_MODEL_BYTES = 12 * 1024;
export const MAX_MODEL_LINES = 200;
const MODEL_TRUNCATION_NOTICE = `[Pi adapter truncated terminal output to the last ${String(MAX_MODEL_LINES)} lines within ${String(MAX_MODEL_BYTES)} bytes]`;
const PHUX_TRUNCATION_NOTICE = "[phux reported that terminal output was already truncated]";

export interface PhuxToolDetails {
  readonly operation: "list" | "panes" | "create" | "spawn" | "launch" | "insert_pane" | "move_pane" | "swap_pane" | "snapshot" | "rendered_snapshot" | "send_keys" | "paste" | "run" | "wait" | "agent_prompt" | "agent_wait" | "resource_wait" | "status" | "runtime_info" | "kill" | "signal" | "tag" | "ask" | "watch" | "targets";
  readonly summary: string;
  readonly target?: string;
  readonly selection?: PhuxTargetSelection;
  readonly count?: number;
  readonly exitCode?: number;
  readonly durationMs?: number;
  readonly outcome?: string;
  readonly result?: Readonly<Record<string, unknown>>;
  readonly warning?: string;
  readonly rows?: number;
  readonly cols?: number;
  readonly modelOutputTruncated?: boolean;
  readonly phuxOutputTruncated?: boolean;
}

/** Register bounded non-shell phux tools against the extension's shared target store. */
export function registerPhuxTools(pi: ExtensionAPI, cli: PhuxCli, store: PhuxTargetStore, parentTerminalId?: string): void {
  const guardWrite = (target: string): void => {
    if (!/^(?:[^/\s]+\/)?@(?:0|[1-9]\d*)$/.test(target)) {
      throw new Error("Writes require a canonical @N or host/@N pane target (or a validated alias/group); raw selectors cannot safely exclude Pi's hosting pane.");
    }
    if (parentTerminalId === undefined) return;
    const parent = /^\d+$/.test(parentTerminalId) ? `@${BigInt(parentTerminalId)}` : parentTerminalId;
    if (!/^(?:[^/\s]+\/)?@(?:0|[1-9]\d*)$/.test(parent)) {
      throw new Error("Cannot safely identify Pi's hosting pane; refusing terminal writes.");
    }
    if (target === parent) throw new Error(`Refusing to write to Pi's hosting pane ${parent}; choose a worker pane.`);
  };
  pi.registerTool({
    name: "phux_paste",
    label: "phux paste",
    description: "Paste literal text, including indentation and newlines, as one paste event into a worker pane. Does not press Enter or execute a command. Use phux_send_keys for a deliberate Enter; use phux_agent_prompt for safe agent-turn delivery. Never automatically retry uncertain delivery.",
    promptSnippet: "Paste multiline text without key-by-key corruption",
    parameters: PhuxPasteParams,
    async execute(_id, params, signal) {
      const target = await resolveActionTarget(params.target, store, signal);
      guardWrite(target);
      await cli.paste(target, params.text, execution(params, signal));
      return toolResult(`Pasted text to ${target}; no Enter was sent.`, { operation: "paste", summary: `pasted to ${target}`, target });
    },
    renderCall: (args, theme) => callText(theme, `paste to ${args.target ?? "selected target"}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_agent_prompt",
    label: "phux agent prompt",
    description: "Submit one single-line agent prompt with acknowledged delivery and optional identity checks. Waits by default for a post-delivery transition to idle, blocked, or done, bounded to 30s. wait:false only submits and forbids until/timeout_seconds. A timeout after acknowledgment was still delivered; on delivery_unknown DO NOT RESEND: inspect the pane.",
    promptSnippet: "Send a prompt to a worker agent and observe its turn",
    parameters: PhuxAgentPromptParams,
    async execute(_id, params, signal) {
      if (params.wait === false && (params.until !== undefined || params.timeout_seconds !== undefined)) {
        throw new Error("until and timeout_seconds require waiting for the agent turn");
      }
      const options = params.wait === false ? execution(params, signal) : operationExecution(params, signal);
      const target = await resolveActionTarget(params.target, store, signal);
      guardWrite(target);
      const result = await cli.agentPrompt(target, params.text, {
        ...options,
        wait: params.wait ?? true,
        ...(params.until === undefined ? {} : { until: params.until }),
        ...(params.expect_agent === undefined ? {} : { expectAgent: params.expect_agent }),
        ...(params.expect_kind === undefined ? {} : { expectKind: params.expect_kind }),
      });
      return recordResult("agent_prompt", result, target);
    },
    renderCall: (args, theme) => callText(theme, `prompt agent ${args.target ?? "selected target"}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_agent_wait",
    label: "phux agent wait",
    description: "Observe a future agent lifecycle transition, bounded to 30s by default. An already-idle agent does not satisfy this; a departed agent is not completion. Prefer phux_agent_prompt with wait to avoid a submit/observe race. Inspect blocked states for required human attention.",
    promptSnippet: "Wait for a worker agent lifecycle state",
    parameters: PhuxAgentWaitParams,
    async execute(_id, params, signal) {
      const options = operationExecution(params, signal);
      const target = await resolveActionTarget(params.target, store, signal);
      const result = await cli.agentWait(target, {
        ...options, ...(params.until === undefined ? {} : { until: params.until }),
      });
      return recordResult("agent_wait", result, target);
    },
    renderCall: (args, theme) => callText(theme, `wait for agent ${args.target ?? "selected target"}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_resource_wait",
    label: "phux resource wait",
    description: "Wait for a canonical resource lifecycle result, optionally after a prior cursor, with a 30s default timeout. Requires an explicit resource identity; never falls back to the selected or focused pane.",
    promptSnippet: "Wait for an explicit phux resource",
    parameters: PhuxResourceWaitParams,
    async execute(_id, params, signal) {
      if (!params.target?.trim() || /^(?:alias|group):/.test(params.target)) throw new Error("resource wait requires an explicit resource identity, not a pane alias/group");
      const result = await cli.resourceWait(params.target, {
        ...operationExecution(params, signal),
        ...(params.after === undefined ? {} : { after: params.after }),
      });
      return recordResult("resource_wait", result, params.target);
    },
    renderCall: (args, theme) => callText(theme, `wait for resource ${args.target}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_status",
    label: "phux status",
    description: "Read bounded canonical phux status JSON. This reports the server, not the branch-local target shown by /phux-status.",
    promptSnippet: "Inspect phux server status",
    parameters: PhuxStatusParams,
    async execute(_id, params, signal) {
      return recordResult("status", await cli.status(execution(params, signal)));
    },
    renderCall: (_args, theme) => callText(theme, "server status"),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_runtime_info",
    label: "phux runtime info",
    description: "Read bounded canonical runtime-info JSON to discover the active runtime and capabilities instead of guessing installed CLI support.",
    promptSnippet: "Inspect phux runtime capabilities",
    parameters: PhuxRuntimeInfoParams,
    async execute(_id, params, signal) {
      return recordResult("runtime_info", await cli.runtimeInfo(execution(params, signal)));
    },
    renderCall: (_args, theme) => callText(theme, "runtime info"),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_list",
    label: "phux list",
    description: "List phux sessions. Output is compact and bounded; this never changes phux focus.",
    promptSnippet: "List shared phux terminal sessions",
    parameters: PhuxListParams,
    async execute(_id, params, signal) {
      const result = await cli.ls(execution(params, signal));
      const lines = result.sessions.map((session) =>
        `${session.name}\twindows=${String(session.windows)}\tattached=${String(session.attached)}`);
      const output = boundedResult(`sessions=${String(result.sessions.length)}`, lines.join("\n") || "No phux sessions.");
      return toolResult(output.text, {
        operation: "list",
        summary: `${String(result.sessions.length)} session(s)`,
        count: result.sessions.length,
        modelOutputTruncated: output.truncated,
      });
    },
    renderCall: (_args, theme) => callText(theme, "list sessions"),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_create",
    label: "phux create",
    description: "Create a named phux session without attaching, then select and persist its seed @id for subsequent tools.",
    promptSnippet: "Create and select a shared phux terminal session",
    parameters: PhuxCreateParams,
    async execute(_id, params, signal) {
      const created = await cli.create(params.name, {
        ...(params.cwd === undefined ? {} : { cwd: params.cwd }),
        ...(params.command === undefined ? {} : { command: params.command }),
        ...execution(params, signal),
      });
      if (created.session !== params.name) {
        throw new Error(`phux new returned session ${JSON.stringify(created.session)}; expected ${JSON.stringify(params.name)}`);
      }
      const selection = store.selectCreated(created.session, created.terminal_id);
      return toolResult(`Created ${created.session} at ${selection.selector}; selected it as the default phux target.`, {
        operation: "create",
        summary: `created ${selection.display}`,
        target: selection.selector,
        selection,
      });
    },
    renderCall: (args, theme) => callText(theme, `create ${args.name}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_snapshot",
    label: "phux snapshot",
    description: "Read a phux pane without attaching or resizing. Uses an explicit target or requires the available selected /phux target. Terminal text is bounded to 200 lines and 12 KiB.",
    promptSnippet: "Read bounded output from a shared phux terminal",
    parameters: PhuxSnapshotParams,
    async execute(_id, params, signal) {
      const target = await resolveActionTarget(params.target, store, signal);
      const screen = await cli.snapshot({
        target,
        ...(params.scrollback === undefined ? {} : { scrollback: params.scrollback }),
        ...(params.cells === undefined ? {} : { cells: params.cells }),
        ...(params.tail === undefined ? {} : { tail: params.tail }),
        ...(params.unwrap === undefined ? {} : { unwrap: params.unwrap }),
        ...execution(params, signal),
      });
      return screenResult("snapshot", target, screen, undefined, undefined, params.cells);
    },
    renderCall: (args, theme) => callText(theme, `snapshot ${args.target ?? "selected target"}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_send_keys",
    label: "phux send keys",
    description: "Send named keys (Enter, C-c, arrows) to an interactive program in a worker pane. Use phux_paste for literal multiline text, phux_run only at a shell prompt, and phux_agent_prompt for an agent turn. Never writes to Pi's hosting pane.",
    promptSnippet: "Send keystrokes to a shared phux terminal",
    parameters: PhuxSendKeysParams,
    async execute(_id, params, signal) {
      const target = await resolveActionTarget(params.target, store, signal);
      guardWrite(target);
      await cli.sendKeys(target, params.keys, execution(params, signal));
      return toolResult(`Sent ${String(params.keys.length)} key item(s) to ${target}.`, {
        operation: "send_keys",
        summary: `sent ${String(params.keys.length)} key item(s) to ${target}`,
        target,
        count: params.keys.length,
      });
    },
    renderCall: (args, theme) => callText(theme, `send ${String(args.keys.length)} key item(s) to ${args.target ?? "selected target"}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_run",
    label: "phux run",
    description: "Run one command line only in a pane known to be at a shell prompt, using phux's run sentinel. Do not send shell syntax to an agent/editor/REPL; use phux_agent_prompt or paste/keys instead. Defaults to a finite 30s wait; nonzero command exits remain command results. Never automatically retry an uncertain delivery.",
    promptSnippet: "Run a command in a shared phux terminal",
    parameters: PhuxRunParams,
    async execute(_id, params, signal) {
      const options = operationExecution(params, signal);
      const target = await resolveActionTarget(params.target, store, signal);
      guardWrite(target);
      const result = await cli.run(target, [params.command], options);
      const header = `run exit=${String(result.exit_code)} duration_ms=${String(result.duration_ms)} target=${target}`;
      const output = boundedResult(header, result.output, result.truncated);
      return toolResult(output.text, {
        operation: "run",
        summary: `exit ${String(result.exit_code)} in ${String(result.duration_ms)} ms on ${target}`,
        target,
        exitCode: result.exit_code,
        durationMs: result.duration_ms,
        modelOutputTruncated: output.truncated,
        phuxOutputTruncated: result.truncated,
      });
    },
    renderCall: (args, theme) => callText(theme, `run ${args.command} on ${args.target ?? "selected target"}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_wait",
    label: "phux wait",
    description: "Wait up to 30s by default for visible literal text, regex, or terminal idleness (choose at most one). output_only filters command echo only with OSC-133 shell integration; a warning reports when filtering is unavailable. tail bounds inspected logical lines. Screen idleness is not proof an agent turn completed: use phux_agent_wait.",
    promptSnippet: "Wait for a condition in a shared phux terminal",
    parameters: PhuxWaitParams,
    async execute(_id, params, signal) {
      if ([params.until, params.regex, params.idle_ms].filter((value) => value !== undefined).length > 1) {
        throw new Error("phux_wait accepts at most one of until, regex, or idle_ms");
      }
      const options = operationExecution(params, signal);
      const target = await resolveActionTarget(params.target, store, signal);
      const result = await cli.wait({
        target,
        ...(params.until === undefined ? {} : { until: params.until }),
        ...(params.regex === undefined ? {} : { regex: params.regex }),
        ...(params.output_only === undefined ? {} : { outputOnly: params.output_only }),
        ...(params.tail === undefined ? {} : { tail: params.tail }),
        ...(params.idle_ms === undefined ? {} : { idleMs: params.idle_ms }),
        ...options,
      });
      return screenResult("wait", target, result.screen, result.outcome, result.warning);
    },
    renderCall: (args, theme) => callText(theme, `wait on ${args.target ?? "selected target"}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_panes",
    label: "phux panes",
    description: "Inventory every pane with canonical selector, ownership, agent state, attention, title, cwd, and evidence. Output is bounded.",
    promptSnippet: "Inventory shared terminal panes and attention state",
    parameters: PhuxPanesParams,
    async execute(_id, _params, signal) {
      const snapshot = await store.refresh(signal);
      if (snapshot.availability === "unavailable") throw new Error(snapshot.reason ?? "phux pane inventory unavailable");
      const lines = store.panes.map((pane) => JSON.stringify({
        terminal: pane.terminal, session: pane.session, window: pane.window,
        agent: pane.agent, state: pane.state, attention: pane.attention,
        confidence: pane.confidence, title: pane.title, cwd: pane.cwd,
        sources: pane.sources, explanation: pane.explanation,
      }));
      const output = boundedResult(`panes=${String(store.panes.length)}`, lines.join("\n") || "No phux panes.");
      return toolResult(output.text, {
        operation: "panes", summary: `${String(store.panes.length)} pane(s)`, count: store.panes.length,
        modelOutputTruncated: output.truncated,
      });
    },
    renderCall: (_args, theme) => callText(theme, "inventory panes"),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_spawn",
    label: "phux spawn",
    description: "Spawn a pane through phux without attaching. Optional target/split/ratio performs local placement; split or ratio requires target and satellite placement is rejected. Optionally bind the validated result to a branch-local alias.",
    promptSnippet: "Spawn, place, and optionally name a shared terminal pane",
    parameters: PhuxSpawnParams,
    async execute(_id, params, signal) {
      const requestedTarget = "target" in params ? params.target : undefined;
      const placementTarget = requestedTarget === undefined
        ? undefined
        : await resolvePlacementTarget(requestedTarget, store, signal);
      const satellite = "satellite" in params ? params.satellite : undefined;
      const split = "split" in params ? params.split : undefined;
      const ratio = "ratio" in params ? params.ratio : undefined;
      validateToolPlacement(placementTarget, split, ratio, satellite);
      const spawned = await cli.spawn({
        ...(satellite === undefined ? {} : { satellite }),
        ...(placementTarget === undefined ? {} : { target: placementTarget }),
        ...(split === undefined ? {} : { split }),
        ...(ratio === undefined ? {} : { ratio }),
        ...(params.cwd === undefined ? {} : { cwd: params.cwd }),
        ...(params.command === undefined ? {} : { command: params.command }),
        ...(params.retain_seconds === undefined ? {} : { retainSeconds: params.retain_seconds }),
        ...execution(params, signal),
      });
      const target = spawned.satellite === null ? `@${String(spawned.terminal_id)}` : `${spawned.satellite}/@${String(spawned.terminal_id)}`;
      const aliasWarning = params.alias === undefined
        ? null
        : await tryBindSpawnedAlias(store, params.alias, target, signal);
      const aliasStatus = params.alias === undefined ? "" : aliasWarning === null
        ? ` as alias:${params.alias}`
        : `; alias:${params.alias} was not saved: ${aliasWarning}`;
      return toolResult(`Spawned ${target}${aliasStatus}.`, {
        operation: "spawn", summary: `spawned ${target}${aliasWarning === null ? "" : " (alias not saved)"}`, target,
      });
    },
    renderCall: (args, theme) => callText(theme, `spawn${"target" in args ? ` beside ${args.target}` : " pane"}${args.alias === undefined ? "" : ` alias:${args.alias}`}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_launch",
    label: "phux launch",
    description: "Launch a configured integration through phux's versioned JSON result, optionally placed beside one exact local pane. Split or ratio requires target. Validates but never displays resolved argv, which may contain sensitive arguments.",
    promptSnippet: "Launch and optionally place a configured agent integration",
    parameters: PhuxLaunchParams,
    async execute(_id, params, signal) {
      const requestedTarget = "target" in params ? params.target : undefined;
      const placementTarget = requestedTarget === undefined
        ? undefined
        : await resolvePlacementTarget(requestedTarget, store, signal);
      const split = "split" in params ? params.split : undefined;
      const ratio = "ratio" in params ? params.ratio : undefined;
      validateToolPlacement(placementTarget, split, ratio);
      const launched = await cli.launch(params.integration, {
        ...(placementTarget === undefined ? {} : { target: placementTarget }),
        ...(split === undefined ? {} : { split }),
        ...(ratio === undefined ? {} : { ratio }),
        ...(params.cwd === undefined ? {} : { cwd: params.cwd }),
        ...(params.extra === undefined ? {} : { extra: params.extra }),
        ...execution(params, signal),
      });
      if (launched.integration !== params.integration) throw new Error("phux launch returned a different integration id");
      const target = `@${String(launched.terminal_id)}`;
      const aliasWarning = params.alias === undefined
        ? null
        : await tryBindSpawnedAlias(store, params.alias, target, signal);
      const aliasStatus = params.alias === undefined ? "" : aliasWarning === null
        ? ` as alias:${params.alias}`
        : `; alias:${params.alias} was not saved: ${aliasWarning}`;
      return toolResult(`Launched ${launched.integration} from plugin ${launched.plugin} at ${target}${aliasStatus}.`, {
        operation: "launch", summary: `launched ${launched.integration} at ${target}${aliasWarning === null ? "" : " (alias not saved)"}`, target,
      });
    },
    renderCall: (args, theme) => callText(theme, `launch ${args.integration}${"target" in args ? ` beside ${args.target}` : ""}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_insert_pane",
    label: "phux insert pane",
    description: "Insert one already-created exact local pane beside another. Both targets are ownership-refreshed aliases or canonical CLI selectors; no spawn and no focus change.",
    promptSnippet: "Insert an existing pane into shared layout topology",
    parameters: PhuxInsertPaneParams,
    async execute(_id, params, signal) {
      const [target, newPane] = await resolveSpatialPair(params.target, params.new_pane, store, signal);
      const result = await cli.insertPane(target, newPane, {
        ...(params.direction === undefined ? {} : { direction: params.direction }),
        ...(params.ratio === undefined ? {} : { ratio: params.ratio }),
        ...execution(params, signal),
      });
      return spatialResult("insert_pane", `inserted @${String(result.new_terminal_id)} beside @${String(result.target_terminal_id)}`, result.session_id, target);
    },
    renderCall: (args, theme) => callText(theme, `insert ${args.new_pane} beside ${args.target}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_move_pane",
    label: "phux move pane",
    description: "Move one exact local pane beside another in the same session. Named aliases are freshly ownership-validated; no focus change.",
    promptSnippet: "Move an existing pane within shared layout topology",
    parameters: PhuxMovePaneParams,
    async execute(_id, params, signal) {
      const [source, target] = await resolveSpatialPair(params.source, params.target, store, signal);
      const result = await cli.movePane(source, target, {
        ...(params.direction === undefined ? {} : { direction: params.direction }),
        ...(params.ratio === undefined ? {} : { ratio: params.ratio }),
        ...execution(params, signal),
      });
      return spatialResult("move_pane", `moved @${String(result.source_terminal_id)} beside @${String(result.target_terminal_id)}`, result.session_id, source);
    },
    renderCall: (args, theme) => callText(theme, `move ${args.source} beside ${args.target}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_swap_pane",
    label: "phux swap pane",
    description: "Swap two exact local pane leaves without changing split geometry or client-local focus. Named aliases are freshly ownership-validated.",
    promptSnippet: "Swap two existing panes in shared layout topology",
    parameters: PhuxSwapPaneParams,
    async execute(_id, params, signal) {
      const [first, second] = await resolveSpatialPair(params.first, params.second, store, signal);
      const result = await cli.swapPane(first, second, execution(params, signal));
      return spatialResult("swap_pane", `swapped @${String(result.first_terminal_id)} and @${String(result.second_terminal_id)}`, result.session_id, first);
    },
    renderCall: (args, theme) => callText(theme, `swap ${args.first} and ${args.second}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_kill",
    label: "phux kill",
    description: "Destroy an explicit canonical pane, validated alias, or every pane in a validated group. confirm:true is required. The entire group is checked against Pi's hosting pane before any destruction; broad raw selectors are rejected.",
    promptSnippet: "Explicitly confirm destruction of shared terminal panes",
    parameters: PhuxKillParams,
    async execute(_id, params, signal) {
      if (params.confirm !== true) throw new Error("phux_kill requires confirm:true; selectors and groups may destroy multiple panes");
      const targets = await resolveActionTargets(params.target, store, signal);
      for (const target of targets) guardWrite(target);
      for (const target of targets) await cli.kill(target, execution(params, signal));
      return controlResult("kill", targets);
    },
    renderCall: (args, theme) => callText(theme, `kill ${args.target}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_signal",
    label: "phux signal",
    description: "Signal a canonical worker pane or validated alias, never Pi's hosting pane. terminate and kill require an explicit target and confirm:true. Use phux_kill to destroy the pane; interrupt/freeze/resume control its process group.",
    promptSnippet: "Signal a process group in a shared terminal",
    parameters: PhuxSignalParams,
    async execute(_id, params, signal) {
      if ((params.signal === "terminate" || params.signal === "kill") && (params.target === undefined || params.confirm !== true)) {
        throw new Error(`phux_signal ${params.signal} requires an explicit target and confirm:true; selectors may affect multiple processes or panes`);
      }
      const target = await resolveActionTarget(params.target, store, signal);
      guardWrite(target);
      await cli.signal(target, params.signal, execution(params, signal));
      return toolResult(`Sent ${params.signal} to ${target}.`, {
        operation: "signal", summary: `${params.signal} ${target}`, target,
      });
    },
    renderCall: (args, theme) => callText(theme, `signal ${args.target ?? "selected target"} ${args.signal}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_tag",
    label: "phux tag",
    description: "List, add, or remove canonical phux tags. group:name expands to bounded, ownership-validated per-pane CLI calls.",
    promptSnippet: "Organize shared terminal panes with tags",
    parameters: PhuxTagParams,
    async execute(_id, params, signal) {
      if (params.action === "ls" && params.tags !== undefined) throw new Error("phux_tag ls does not accept tags");
      if (params.action !== "ls" && params.tags === undefined) throw new Error(`phux_tag ${params.action} requires tags`);
      const targets = await resolveActionTargets(params.target, store, signal);
      if (params.action !== "ls") for (const target of targets) guardWrite(target);
      const rows = [];
      for (const target of targets) rows.push(...await cli.tag(params.action, target, params.tags ?? [], execution(params, signal)));
      const output = boundedResult(`tag ${params.action} targets=${String(targets.length)}`, rows.map((row) => `${row.terminal}\t${row.tagsText}`).join("\n"));
      return toolResult(output.text, {
        operation: "tag", summary: `tag ${params.action} on ${String(targets.length)} target(s)`, count: targets.length,
        modelOutputTruncated: output.truncated,
      });
    },
    renderCall: (args, theme) => callText(theme, `tag ${args.action} ${args.target}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_ask",
    label: "phux ask",
    description: "Report a bounded human-attention ask event for one pane through phux. Pane attention is visible in phux_panes and phux_watch_events.",
    promptSnippet: "Report that an agent needs human attention",
    parameters: PhuxAskParams,
    async execute(_id, params, signal) {
      const target = await resolveActionTarget(params.target, store, signal);
      guardWrite(target);
      const asked = await cli.ask(target, params.question, {
        ...(params.id === undefined ? {} : { id: params.id }),
        ...(params.suggestions === undefined ? {} : { suggestions: params.suggestions }),
        ...(params.elapsed_seconds === undefined ? {} : { elapsedSeconds: params.elapsed_seconds }),
        ...execution(params, signal),
      });
      return toolResult(`Reported ask ${JSON.stringify(asked.id)} for ${asked.terminal}.`, {
        operation: "ask", summary: `attention requested on ${asked.terminal}`, target: asked.terminal,
      });
    },
    renderCall: (args, theme) => callText(theme, `ask ${args.target ?? "selected target"}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_watch_events",
    label: "phux watch events",
    description: "Collect events for a required bounded duration, then terminate phux watch. Returns at most max_events and never leaves an indefinite subprocess.",
    promptSnippet: "Wait briefly for bounded shared-terminal events",
    parameters: PhuxWatchParams,
    async execute(_id, params, signal) {
      const target = await resolveActionTarget(params.target, store, signal);
      const collection = await cli.watch({ target, durationMs: params.duration_ms, maxEvents: params.max_events ?? 50, ...(signal === undefined ? {} : { signal }) });
      const output = boundedResult(
        `watch target=${target} events=${String(collection.events.length)} ended=${String(collection.ended)} collection_truncated=${String(collection.truncated)}`,
        collection.events.map((event) => JSON.stringify(event)).join("\n"),
      );
      return toolResult(output.text, {
        operation: "watch", summary: `collected ${String(collection.events.length)} event(s) on ${target}`, target,
        count: collection.events.length, modelOutputTruncated: output.truncated || collection.truncated,
      });
    },
    renderCall: (args, theme) => callText(theme, `watch ${args.target ?? "selected target"} for ${String(args.duration_ms)} ms`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_rendered_snapshot",
    label: "phux rendered snapshot",
    description: "Capture the client's composited multi-pane frame at bounded dimensions. This canonical CLI mode attaches headlessly and may resize its client view.",
    promptSnippet: "Inspect the composited shared-terminal client view",
    parameters: PhuxRenderedSnapshotParams,
    async execute(_id, params, signal) {
      const frame = await cli.renderedSnapshot({
        ...(params.session === undefined ? {} : { session: params.session }),
        cols: params.cols ?? 80, rows: params.rows ?? 24,
        ...execution(params, signal),
      });
      const lines = Array.from({ length: frame.rows }, (_, row) => frame.cells
        .slice(row * frame.cols, (row + 1) * frame.cols)
        .map((cell) => sanitizeRenderText(cell.grapheme)).join(""));
      const output = boundedResult(`rendered session=${params.session ?? "last"} size=${String(frame.cols)}x${String(frame.rows)}`, lines.join("\n"));
      return toolResult(output.text, {
        operation: "rendered_snapshot", summary: `rendered ${String(frame.cols)}x${String(frame.rows)}`, rows: frame.rows, cols: frame.cols,
        modelOutputTruncated: output.truncated,
      });
    },
    renderCall: (args, theme) => callText(theme, `rendered snapshot ${args.session ?? "last session"}`),
    renderResult: renderSummary,
  });

  pi.registerTool({
    name: "phux_targets",
    label: "phux targets",
    description: "List or mutate branch-local named aliases and groups. Definitions persist in Pi session history with canonical pane ownership; alias:name and group:name never silently retarget reused ids.",
    promptSnippet: "Manage branch-local named terminal targets",
    parameters: PhuxTargetsParams,
    async execute(_id, params, signal) {
      if (params.action === "set_alias" || params.action === "set_group") {
        const snapshot = await store.refresh(signal);
        if (snapshot.availability === "unavailable") throw new Error(snapshot.reason ?? "phux pane inventory unavailable");
      }
      switch (params.action) {
        case "set_alias": {
          const targets = resolveTargets(params.target, store);
          if (targets.length !== 1) throw new Error("an alias must resolve to exactly one pane");
          const selection = store.selectionFor(targets[0] ?? "");
          if (selection === null) throw new Error("target is not present in the current pane inventory");
          store.bindAlias(params.name, selection);
          break;
        }
        case "remove_alias": store.removeAlias(params.name); break;
        case "set_group": {
          const selectors = params.targets.flatMap((target) => resolveTargets(target, store));
          const selections = selectors.map((selector) => store.selectionFor(selector));
          if (selections.some((selection) => selection === null)) throw new Error("every group target must be present in the current pane inventory");
          store.bindGroup(params.name, selections as PhuxTargetSelection[]);
          break;
        }
        case "remove_group": store.removeGroup(params.name); break;
        case "list": break;
      }
      const aliases = Object.entries(store.named.aliases).map(([name, selection]) => `alias:${name}\t${selection.display}`);
      const groups = Object.entries(store.named.groups).map(([name, selections]) => `group:${name}\t${selections.map((selection) => selection.selector).join(",")}`);
      const output = boundedResult(`aliases=${String(aliases.length)} groups=${String(groups.length)}`, [...aliases, ...groups].join("\n") || "No named phux targets.");
      return toolResult(output.text, {
        operation: "targets", summary: `${String(aliases.length)} alias(es), ${String(groups.length)} group(s)`,
        count: aliases.length + groups.length, modelOutputTruncated: output.truncated,
      });
    },
    renderCall: (args, theme) => callText(theme, `targets ${args.action}`),
    renderResult: renderSummary,
  });
}

function execution(params: { readonly local_timeout_ms?: number }, signal: AbortSignal | undefined, defaultMs = 10_000) {
  signal?.throwIfAborted();
  const timeoutMs = params.local_timeout_ms ?? defaultMs;
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 86_405_000) {
    throw new RangeError("local_timeout_ms must be an integer from 1 through 86405000");
  }
  return { ...(signal === undefined ? {} : { signal }), timeoutMs };
}

function operationExecution(params: { readonly local_timeout_ms?: number; readonly timeout_seconds?: number }, signal: AbortSignal | undefined) {
  const phuxTimeoutSeconds = operationTimeout(params.timeout_seconds);
  const deadline = phuxTimeoutSeconds * 1000 + 5000;
  return { ...execution(params, signal, deadline), phuxTimeoutSeconds };
}

export function resolveTarget(explicit: string | undefined, store: PhuxTargetStore): string {
  if (explicit?.startsWith("group:") === true) {
    throw new Error("A target group cannot be used for a single-pane operation.");
  }
  if (explicit?.startsWith("alias:") === true) return store.resolveAlias(explicit.slice("alias:".length)).selector;
  if (explicit !== undefined) return explicit;
  const snapshot = store.snapshot;
  if (snapshot.selection === null) {
    throw new Error("No phux target is selected. Pass target explicitly or run /phux to select an available pane.");
  }
  if (snapshot.availability !== "available") {
    throw new Error(`Selected phux target ${snapshot.selection.selector} is ${snapshot.availability}${snapshot.reason === undefined ? "" : `: ${snapshot.reason}`}. Pass target explicitly or run /phux to select an available pane.`);
  }
  return snapshot.selection.selector;
}

export function resolveTargets(explicit: string | undefined, store: PhuxTargetStore): readonly string[] {
  if (explicit?.startsWith("group:") === true) {
    return store.resolveGroup(explicit.slice("group:".length)).map((selection) => selection.selector);
  }
  return [resolveTarget(explicit, store)];
}

const SATELLITE_PANE = /^[^/\s]+\/@\d+$/;

async function resolvePlacementTarget(
  explicit: string,
  store: PhuxTargetStore,
  signal: AbortSignal | undefined,
): Promise<string> {
  const target = await resolveActionTarget(explicit, store, signal);
  if (SATELLITE_PANE.test(target)) {
    throw new Error("explicit placement is local-only; satellite pane targets are unsupported");
  }
  return target;
}

async function resolveSpatialPair(
  first: string,
  second: string,
  store: PhuxTargetStore,
  signal: AbortSignal | undefined,
): Promise<readonly [string, string]> {
  if (first.startsWith("alias:") || second.startsWith("alias:")) {
    await refreshNamedTargets(store, signal);
  }
  const resolvedFirst = resolveTarget(first, store);
  const resolvedSecond = resolveTarget(second, store);
  if (SATELLITE_PANE.test(resolvedFirst) || SATELLITE_PANE.test(resolvedSecond)) {
    throw new Error("spatial actions require exact local pane targets");
  }
  if (resolvedFirst === resolvedSecond) {
    throw new Error("spatial actions require two distinct pane targets");
  }
  return [resolvedFirst, resolvedSecond];
}

function validateToolPlacement(
  target: string | undefined,
  split: "horizontal" | "vertical" | undefined,
  ratio: number | undefined,
  satellite?: string,
): void {
  if (target === undefined && (split !== undefined || ratio !== undefined)) {
    throw new Error("target is required when split or ratio is provided");
  }
  if (target !== undefined && satellite !== undefined) {
    throw new Error("satellite and target placement cannot be combined");
  }
}

async function resolveActionTarget(
  explicit: string | undefined,
  store: PhuxTargetStore,
  signal: AbortSignal | undefined,
): Promise<string> {
  signal?.throwIfAborted();
  if ((explicit === undefined && store.snapshot.selection !== null) || explicit?.startsWith("alias:") === true) await refreshNamedTargets(store, signal);
  return resolveTarget(explicit, store);
}

async function resolveActionTargets(
  explicit: string | undefined,
  store: PhuxTargetStore,
  signal: AbortSignal | undefined,
): Promise<readonly string[]> {
  signal?.throwIfAborted();
  if (explicit === undefined) throw new Error("an explicit target is required");
  if (explicit.startsWith("alias:") || explicit.startsWith("group:")) await refreshNamedTargets(store, signal);
  return resolveTargets(explicit, store);
}

async function refreshNamedTargets(store: PhuxTargetStore, signal: AbortSignal | undefined): Promise<void> {
  const snapshot = await store.refresh(signal);
  if (snapshot.availability === "unavailable") {
    throw new Error(snapshot.reason ?? "phux pane inventory unavailable; named target resolution failed closed");
  }
}

async function tryBindSpawnedAlias(
  store: PhuxTargetStore,
  alias: string,
  target: string,
  signal: AbortSignal | undefined,
): Promise<string | null> {
  const snapshot = await store.refresh(signal);
  if (snapshot.availability === "unavailable") {
    return snapshot.reason ?? "could not validate spawned pane ownership";
  }
  const selection = store.selectionFor(target);
  if (selection === null) return `spawned pane ${target} was not present in the canonical pane inventory`;
  store.bindAlias(alias, selection);
  return null;
}

function spatialResult(
  operation: "insert_pane" | "move_pane" | "swap_pane",
  summary: string,
  sessionId: number,
  target: string,
): AgentToolResult<PhuxToolDetails> {
  return toolResult(`${summary} in session ${String(sessionId)}.`, {
    operation,
    summary,
    target,
  });
}

function controlResult(operation: "kill", targets: readonly string[]): AgentToolResult<PhuxToolDetails> {
  return toolResult(`Killed ${String(targets.length)} target(s): ${targets.join(", ")}.`, {
    operation, summary: `killed ${String(targets.length)} target(s)`, count: targets.length,
    ...(targets.length === 1 ? { target: targets[0] } : {}),
  });
}

function screenResult(
  operation: "snapshot" | "wait",
  target: string,
  screen: ScreenState,
  outcome?: string,
  warning?: string,
  cells?: boolean,
): AgentToolResult<PhuxToolDetails> {
  const terminal = cells ? JSON.stringify(screen, null, 2) : [...screen.scrollback, ...screen.lines].join("\n");
  const header = `${operation}${outcome === undefined ? "" : ` ${outcome}`} target=${target} pane=@${String(screen.pane)} size=${String(screen.cols)}x${String(screen.rows)}`;
  const output = boundedResult(header, warning === undefined ? terminal : `${terminal}\nCLI warning: ${warning}`, screen.truncated);
  return toolResult(output.text, {
    operation,
    summary: `${operation}${outcome === undefined ? "" : ` ${outcome}`} on ${target} (${String(screen.cols)}x${String(screen.rows)})`,
    target,
    ...(outcome === undefined ? {} : { outcome }),
    ...(warning === undefined ? {} : { warning }),
    rows: screen.rows,
    cols: screen.cols,
    modelOutputTruncated: output.truncated,
    phuxOutputTruncated: screen.truncated ?? false,
  });
}

function operationTimeout(value: number | undefined): number {
  const timeout = value ?? 30;
  if (!Number.isSafeInteger(timeout) || timeout < 1 || timeout > 86_400) {
    throw new RangeError("timeout_seconds must be an integer from 1 through 86400; unbounded waits are not supported");
  }
  return timeout;
}

function recordResult(
  operation: "agent_prompt" | "agent_wait" | "resource_wait" | "status" | "runtime_info",
  result: Readonly<Record<string, unknown>>,
  target?: string,
): AgentToolResult<PhuxToolDetails> {
  const outcome = typeof result.outcome === "string" ? result.outcome : undefined;
  const summary = `${operation}${outcome === undefined ? "" : ` ${outcome}`}${target === undefined ? "" : ` on ${target}`}`;
  const output = boundedResult(summary, JSON.stringify(result, null, 2));
  return toolResult(output.text, {
    operation, summary,
    ...(output.truncated ? {} : { result }),
    ...(target === undefined ? {} : { target }),
    ...(outcome === undefined ? {} : { outcome }),
    modelOutputTruncated: output.truncated,
  });
}

/** Bound only terminal body text so the result header and truncation notices remain visible. */
export function boundedResult(
  header: string,
  body: string,
  phuxTruncated = false,
): { readonly text: string; readonly truncated: boolean } {
  const reservedNotices = [MODEL_TRUNCATION_NOTICE, ...(phuxTruncated ? [PHUX_TRUNCATION_NOTICE] : [])];
  const separators = 1 + reservedNotices.length;
  const reservedBytes = Buffer.byteLength(header) +
    reservedNotices.reduce((total, notice) => total + Buffer.byteLength(notice), 0) + separators;
  const bodyResult = truncateTail(body, {
    maxBytes: Math.max(0, MAX_MODEL_BYTES - reservedBytes),
    maxLines: Math.max(0, MAX_MODEL_LINES - 1 - reservedNotices.length),
  });
  const notices = [
    ...(bodyResult.truncated ? [MODEL_TRUNCATION_NOTICE] : []),
    ...(phuxTruncated ? [PHUX_TRUNCATION_NOTICE] : []),
  ];
  return {
    text: [header, ...(bodyResult.content.length === 0 ? [] : [bodyResult.content]), ...notices].join("\n"),
    truncated: bodyResult.truncated,
  };
}

function toolResult(text: string, details: PhuxToolDetails): AgentToolResult<PhuxToolDetails> {
  return { content: [{ type: "text", text }], details };
}

export function sanitizeRenderText(text: string): string {
  return text
    .replace(/\x1B\][^\x07]*(?:\x07|\x1B\\)/g, "")
    .replace(/(?:\x1B\[|\x9B)[0-?]*[ -/]*[@-~]/g, "")
    .replace(/\x1B[@-_]/g, "")
    .replace(/[\x00-\x1F\x7F-\x9F]/g, " ");
}

function callText(theme: Theme, text: string): Text {
  const safe = sanitizeRenderText(`phux ${text}`);
  return new Text(theme.fg("toolTitle", theme.bold(safe)), 0, 0);
}

function renderSummary(
  result: AgentToolResult<PhuxToolDetails>,
  _options: unknown,
  theme: Theme,
): Text {
  const details = result.details;
  if (details === undefined) return new Text(theme.fg("error", "phux tool failed"), 0, 0);
  const suffix = details.modelOutputTruncated ? " (model output truncated)" : "";
  const safe = sanitizeRenderText(`${details.summary}${suffix}`);
  return new Text(theme.fg(details.exitCode === undefined || details.exitCode === 0 ? "success" : "warning", safe), 0, 0);
}
