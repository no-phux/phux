import { Plugin } from "@opencode/plugin";

import { PhuxCli, type PhuxCliOptions } from "../../runtime/src/adapter.js";
import { PhuxScreen } from "../../runtime/src/screen.js";
import {
  PhuxContextAwareness,
  contextAwarenessEnabled,
  normalizeTerminalIdentity,
} from "../../runtime/src/awareness.js";
import { applyServerEvent, applyV1Event, deletedSessionId } from "./events.js";
import { OpenCodeLifecycle } from "./lifecycle.js";
import { createPhuxTools, type PhuxToolDefinition, type ToolContext, type ToolResult } from "../../runtime/src/tools.js";
import { PARENT_PANE_RULE, parentPane } from "./parent.js";
import { DEFAULT_MAX_TERMINALS, OwnedTerminals } from "./terminals.js";
import { createGuiService, registerTerminals } from "./gui-service.js";
import {
  PhuxRpc,
  type InputInput,
  type KillInput,
  type OverviewInput,
  type PromptInput,
  type ScreenInput,
  type SpawnInput,
} from "./rpc.js";


export interface PhuxOpenCodeV2Options {
  readonly executable?: string;
  readonly socket?: string;
  readonly lifecycleTimeoutMs?: number;
  /** Keep a fleet inventory in each session's context. Off by default: phux_panes answers on demand. */
  readonly contextAwareness?: boolean;
  readonly contextTimeoutMs?: number;
  /** Live terminals one OpenCode session may own through phux_spawn. */
  readonly maxTerminals?: number;
  /** Only for a dedicated pane-bound server; ordinary presence belongs to the TUI. */
  readonly serverLifecycle?: boolean;
}

/**
 * OpenCode V2 server plugin.
 *
 * OpenCode's built-in shell stays a local subprocess for ordinary commands.
 * phux supplies terminals only where a terminal is the point: interactive
 * programs, processes the user watches, and other agents' panes. Panes the
 * agent spawns are owned by its session and closed with it.
 */
const plugin = Plugin.define({
  id: "phux",
  async setup(ctx) {
    const options = readOptions(ctx.options);
    const environment = process.env;
    const reportLifecycle = environment.PHUX_AGENT_REPORTING !== "0" && options.serverLifecycle === true;
    const environmentTarget = readEnvironmentTarget(environment.PHUX_TARGET);
    const parent = parentPane(environment.PHUX_TERMINAL_ID);
    const cli = new PhuxCli(cliOptions(options, environment));
    const selectedTargets = new Map<string, string>();
    // The hosting pane is identity; controlled targets never become this agent.
    const identityTarget = parent ?? environmentTarget;
    const lifecycle = new OpenCodeLifecycle({
      cli,
      target: () => identityTarget,
      ...(options.lifecycleTimeoutMs === undefined ? {} : { timeoutMs: options.lifecycleTimeoutMs }),
    });
    const contextAwareness = options.contextAwareness ?? contextAwarenessEnabled(environment.PHUX_CONTEXT_AWARENESS, false);
    const awareness = new PhuxContextAwareness(cli, {
      enabled: contextAwareness,
      ...(options.contextTimeoutMs === undefined ? {} : { timeoutMs: options.contextTimeoutMs }),
    });
    const maxTerminals = options.maxTerminals ?? DEFAULT_MAX_TERMINALS;
    const terminals = new OwnedTerminals(cli, maxTerminals);
    const unregisterTerminals = registerTerminals(terminals);
    const contextIdentity = (sessionID: string) => {
      const self = normalizeTerminalIdentity(identityTarget);
      const selected = selectedTargets.get(sessionID) ?? environmentTarget;
      return {
        ...(self === null ? {} : { self }),
        ...(selected === undefined ? {} : { selected }),
      };
    };

    const shared = createPhuxTools({
      cli,
      ...(environmentTarget === undefined ? {} : { environmentTarget }),
      ...(parent === null ? {} : { parentTarget: parent }),
      getSelectedTarget: (context) => context === undefined ? undefined : selectedTargets.get(context.sessionID),
      selectTarget: (target, context) => {
        if (context !== undefined) selectedTargets.set(context.sessionID, target);
      },
    });
    const tools = [
      ...SHARED_TOOLS.map((name) => shared[name]!),
      ownedSpawn(shared.phux_spawn!, terminals, selectedTargets),
      killTool(terminals, selectedTargets),
    ];

    await ctx.tool.transform((editor) => {
      for (const tool of tools) {
        editor.add({
          name: tool.name,
          description: tool.description,
          input: tool.input,
          options: { codemode: true, permission: tool.name },
          execute: async (input, context) => withImages(await tool.execute(input, context)),
        });
      }
    });

    await ctx.session.hook("context", async (event) => {
      event.system.push({ type: "text", text: PARENT_PANE_RULE });
    });

    if (contextAwareness) {
      // Fleet state is a session instruction entry, never per-request system
      // text: OpenCode keeps the first value in the session's system prompt and
      // appends each later change to history as a durable system message, and
      // carries it through compaction and forks. A fleet change therefore never
      // rewrites the cached prefix, and an unchanged value is a no-op. It is
      // refreshed as each prompt is admitted, before that turn's first request.
      await ctx.session.hook("prompt", async (event) => {
        const value = await awareness.state(contextIdentity(event.sessionID));
        if (value === null) return;
        await ctx.session.instructions.entry
          .put({ sessionID: event.sessionID, key: FLEET_CONTEXT_KEY, value })
          .catch((error: unknown) => console.error("phux fleet context:", error));
      });
    }

    if (reportLifecycle) {
      await ctx.tool.hook("execute.before", async (event) => {
        await lifecycle.toolStart(event.sessionID, event.tool, event.id);
      });
      await ctx.tool.hook("execute.after", async (event) => {
        await lifecycle.toolEnd(event.sessionID, event.tool, event.id);
      });
    }

    // The desktop and web GUI reach phux only through this RPC, so the CLI,
    // its socket, and each session's terminal ownership stay server-side.
    const screen = new PhuxScreen(cliOptions(options, environment));
    const gui = createGuiService({ cli, screen, terminals, maxTerminals, parent, selectedTargets });
    const rpc = await ctx.rpc.register(PhuxRpc, {
      overview: (input) => gui.overview(input as OverviewInput),
      screen: (input, call) => gui.screen(input as ScreenInput, { signal: call.signal }),
      input: (input, call) => gui.input(input as InputInput, { signal: call.signal }),
      prompt: (input, call) => gui.prompt(input as PromptInput, { signal: call.signal }),
      spawn: (input, call) => gui.spawn(input as SpawnInput, { signal: call.signal }),
      kill: (input, call) => gui.kill(input as KillInput, { signal: call.signal }),
    });

    const controller = new AbortController();
    const events = (async () => {
      for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
        const deleted = deletedSessionId(event);
        if (deleted !== undefined) {
          selectedTargets.delete(deleted);
          await terminals.release(deleted);
        }
        if (reportLifecycle) await applyServerEvent(lifecycle, event);
      }
    })();
    void events.catch((error: unknown) => {
      if (controller.signal.aborted) return;
      console.error("phux plugin event stream ended", error);
    });

    return async () => {
      controller.abort();
      await rpc.dispose();
      unregisterTerminals();
      await events.catch(() => undefined);
      if (reportLifecycle) await lifecycle.dispose();
      await terminals.releaseAll();
      selectedTargets.clear();
    };
  },
});

/**
 * OpenCode 1.x server plugin: 1.x calls `server`, 2.2+ calls `setup`, so one
 * definition serves both. 1.x has no TUI plugin for phux, so this reports
 * the hosting pane's sessions, state and asks, and registers `phux mcp` for
 * the phux tools unless the config already names a `phux` MCP server.
 */
async function server(_input: unknown, value?: unknown): Promise<V1Hooks> {
  const options = readOptions(value);
  const config: V1Hooks["config"] = async (cfg) => {
    cfg.mcp ??= {};
    cfg.mcp.phux ??= { type: "local", command: [options.executable ?? "phux", "mcp"] };
  };
  const parent = parentPane(process.env.PHUX_TERMINAL_ID);
  // A shared `opencode serve` inherits whichever pane started it.
  if (process.env.PHUX_AGENT_REPORTING === "0" || parent === null || process.argv.includes("serve")) return { config };
  const lifecycle = new OpenCodeLifecycle({
    cli: new PhuxCli(cliOptions(options, process.env)),
    target: () => parent,
    ...(options.lifecycleTimeoutMs === undefined ? {} : { timeoutMs: options.lifecycleTimeoutMs }),
    onError: (error) => console.error("phux OpenCode presence:", error),
  });
  return {
    config,
    event: ({ event }) => applyV1Event(lifecycle, event),
    "tool.execute.before": (input) => lifecycle.toolStart(input.sessionID, input.tool, input.callID),
    "tool.execute.after": (input) => lifecycle.toolEnd(input.sessionID, input.tool, input.callID),
    dispose: () => lifecycle.dispose(),
  };
}

/** The OpenCode 1.x hooks this plugin uses (`@opencode-ai/plugin` `Hooks`). */
interface V1Hooks {
  config?: (cfg: { mcp?: Record<string, unknown> }) => Promise<void>;
  event?: (input: { event: { type?: unknown; properties?: unknown } }) => Promise<void>;
  "tool.execute.before"?: (input: { tool: string; sessionID: string; callID: string }) => Promise<void>;
  "tool.execute.after"?: (input: { tool: string; sessionID: string; callID: string }) => Promise<void>;
  dispose?: () => Promise<void>;
}

export default { ...plugin, server };

/**
 * The shared catalog minus what OpenCode already does better or what invites
 * misuse: phux_create made a named session per task, and phux_list and
 * phux_runtime_info duplicate phux_panes, phux_documents and phux_status.
 */
const SHARED_TOOLS = [
  "phux_panes",
  "phux_run",
  "phux_send_keys",
  "phux_paste",
  "phux_snapshot",
  "phux_wait",
  "phux_agent_prompt",
  "phux_agent_wait",
  "phux_resource_wait",
  "phux_status",
  "phux_documents",
  "phux_tree_snapshot",
  "phux_tree_wait",
  "phux_surface_snapshot",
] as const;

/** A surface frame reaches the model as a data-URL file part beside the text. */
function withImages(result: ToolResult) {
  if (result.images === undefined || result.images.length === 0) return { content: result.content, metadata: result.metadata };
  const files = result.images.map((image, index) => ({
    type: "file" as const,
    uri: `data:${image.mimeType};base64,${image.data}`,
    mime: image.mimeType,
    name: `frame-${index + 1}.${image.mimeType === "image/png" ? "png" : "jpg"}`,
  }));
  return { content: [{ type: "text" as const, text: result.content }, ...files], metadata: result.metadata };
}

/** The instruction entry key OpenCode renders as `<context key="phux-fleet">`. */
const FLEET_CONTEXT_KEY = "phux-fleet";

const TARGET = { type: "string", minLength: 2, maxLength: 512, pattern: "^(?:[^/\\s]+/)?@[0-9]+$", description: "Exact @N or host/@N returned by phux_spawn." };

function ownedSpawn(
  spawn: PhuxToolDefinition<any>,
  terminals: OwnedTerminals,
  selectedTargets: Map<string, string>,
): PhuxToolDefinition<any> {
  return {
    ...spawn,
    description:
      "Open a terminal pane for an interactive program (REPL, TUI, debugger) or a long-running process the user should watch. " +
      "Not for ordinary commands: use the shell tool. command is argv, not shell text. " +
      "The pane becomes this session's default phux target, counts against a per-session cap, and is closed by phux_kill or when the session is deleted. " +
      "Set retain_seconds to keep output and exit status for phux_resource_wait after exit; target places it beside an exact pane.",
    async execute(input, context: ToolContext) {
      await terminals.admit(context.sessionID, context.signal);
      const result = await spawn.execute(input, context);
      const target = result.metadata.target;
      if (typeof target === "string") {
        await terminals.add(context.sessionID, target);
        selectedTargets.set(context.sessionID, target);
      }
      return result;
    },
  };
}

function killTool(terminals: OwnedTerminals, selectedTargets: Map<string, string>): PhuxToolDefinition<any> {
  return {
    name: "phux_kill",
    description: "Close a terminal this session opened with phux_spawn. Refuses panes it did not spawn.",
    input: { type: "object", properties: { target: TARGET }, required: ["target"], additionalProperties: false },
    async execute(input: { target: string }, context: ToolContext) {
      await terminals.kill(context.sessionID, input.target, context.signal);
      if (selectedTargets.get(context.sessionID) === input.target) selectedTargets.delete(context.sessionID);
      return { content: `kill target=${input.target}\nclosed`, metadata: { operation: "kill", target: input.target } };
    },
  };
}

function readOptions(value: unknown): PhuxOpenCodeV2Options {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return {};
  const record = value as Record<string, unknown>;
  return {
    ...(typeof record.executable === "string" ? { executable: record.executable } : {}),
    ...(typeof record.socket === "string" ? { socket: record.socket } : {}),
    ...(typeof record.lifecycleTimeoutMs === "number" ? { lifecycleTimeoutMs: record.lifecycleTimeoutMs } : {}),
    ...(typeof record.contextAwareness === "boolean" ? { contextAwareness: record.contextAwareness } : {}),
    ...(typeof record.contextTimeoutMs === "number" ? { contextTimeoutMs: record.contextTimeoutMs } : {}),
    ...(typeof record.serverLifecycle === "boolean" ? { serverLifecycle: record.serverLifecycle } : {}),
    ...(typeof record.maxTerminals === "number" ? { maxTerminals: record.maxTerminals } : {}),
  };
}

function cliOptions(options: PhuxOpenCodeV2Options, environment: NodeJS.ProcessEnv): PhuxCliOptions {
  return {
    ...(options.executable === undefined ? {} : { executable: options.executable }),
    ...(options.socket === undefined ? {} : { socket: options.socket }),
    env: environment,
  };
}

function readEnvironmentTarget(value: string | undefined): string | undefined {
  if (value === undefined || value.trim().length === 0) return undefined;
  if (value.length > 512) throw new RangeError("PHUX_TARGET must be at most 512 characters");
  return value;
}
