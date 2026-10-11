import { Plugin } from "@opencode/plugin";

import { PhuxCli, type PhuxCliOptions } from "../../runtime/src/adapter.js";
import {
  PhuxContextAwareness,
  contextAwarenessEnabled,
  normalizeTerminalIdentity,
} from "../../runtime/src/awareness.js";
import { deletedSessionId, applyServerEvent } from "./events.js";
import { OpenCodeLifecycle } from "./lifecycle.js";
import { createPhuxTools, type PhuxToolDefinition, type ToolContext } from "../../runtime/src/tools.js";
import { PARENT_PANE_RULE, parentPane } from "./parent.js";
import { DEFAULT_MAX_TERMINALS, OwnedTerminals } from "./terminals.js";


export interface PhuxOpenCodeV2Options {
  readonly executable?: string;
  readonly socket?: string;
  readonly lifecycleTimeoutMs?: number;
  /** Inject a fleet inventory into every request. Off by default: phux_panes answers on demand. */
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
export default Plugin.define({
  id: "phux",
  async setup(ctx) {
    const options = readOptions(ctx.options);
    const environment = process.env;
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
    const awareness = new PhuxContextAwareness(cli, {
      enabled: options.contextAwareness ?? contextAwarenessEnabled(environment.PHUX_CONTEXT_AWARENESS, false),
      ...(options.contextTimeoutMs === undefined ? {} : { timeoutMs: options.contextTimeoutMs }),
    });
    const terminals = new OwnedTerminals(cli, options.maxTerminals ?? DEFAULT_MAX_TERMINALS);
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
          execute: (input, context) => tool.execute(input, context),
        });
      }
    });

    await ctx.session.hook("context", async (event) => {
      event.system.push({ type: "text", text: PARENT_PANE_RULE });
      // The system prompt is rebuilt per request, so only a full checkpoint is
      // meaningful here; a delta would arrive without its base.
      const emission = await awareness.checkpoint(event.sessionID, contextIdentity(event.sessionID));
      if (emission !== null) event.system.push({ type: "text", text: emission.text });
    });

    if (options.serverLifecycle === true) {
      await ctx.tool.hook("execute.before", async (event) => {
        await lifecycle.toolStart(event.sessionID, event.tool, event.id);
      });
      await ctx.tool.hook("execute.after", async (event) => {
        await lifecycle.toolEnd(event.sessionID, event.tool, event.id);
      });
    }

    const controller = new AbortController();
    const events = (async () => {
      for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
        const deleted = deletedSessionId(event);
        if (deleted !== undefined) {
          awareness.delete(deleted);
          selectedTargets.delete(deleted);
          await terminals.release(deleted);
        }
        if (options.serverLifecycle === true) await applyServerEvent(lifecycle, event);
      }
    })();
    void events.catch((error: unknown) => {
      if (controller.signal.aborted) return;
      console.error("phux plugin event stream ended", error);
    });

    return async () => {
      controller.abort();
      await events.catch(() => undefined);
      if (options.serverLifecycle === true) await lifecycle.dispose();
      await terminals.releaseAll();
      selectedTargets.clear();
    };
  },
});

/**
 * The shared catalog minus what OpenCode already does better or what invites
 * misuse: phux_create made a named session per task, and phux_list and
 * phux_runtime_info duplicate phux_panes and phux_status.
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
] as const;

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
