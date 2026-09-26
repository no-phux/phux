import { Plugin } from "@opencode/plugin";

import { PhuxCli, type PhuxCliOptions } from "../../runtime/src/adapter.js";
import {
  PhuxContextAwareness,
  contextAwarenessEnabled,
  normalizeTerminalIdentity,
} from "../../runtime/src/awareness.js";
import { OpenCodeLifecycle, handleLifecycleEvent } from "../../opencode/src/lifecycle.js";
import type { ToolContext } from "../../opencode/src/tools.js";
import { deletedSessionId, toLifecycleEvent } from "./events.js";
import { PARENT_PANE_RULE, parentPane } from "./parent.js";
import { createGuardedTools } from "./tools.js";

export { PARENT_PANE_RULE, parentPane, samePane } from "./parent.js";
export { toLifecycleEvent, deletedSessionId } from "./events.js";
export { createGuardedTools } from "./tools.js";

export interface PhuxOpenCodeV2Options {
  readonly executable?: string;
  readonly socket?: string;
  readonly lifecycleTimeoutMs?: number;
  readonly contextAwareness?: boolean;
  readonly contextTimeoutMs?: number;
}

/**
 * OpenCode V2 server plugin.
 *
 * phux owns the PTY. This plugin is the AgentSession producer and the tool
 * surface for sibling terminals. It does not create an OpenCode PTY and it
 * does not dial a remote OpenCode server.
 */
export default Plugin.define({
  id: "phux",
  async setup(ctx) {
    const options = readOptions(ctx.options);
    const environment = process.env;
    const environmentTarget = readEnvironmentTarget(environment.PHUX_TARGET);
    const parent = parentPane(environment.PHUX_TERMINAL_ID);
    const cli = new PhuxCli(cliOptions(options, environment));
    let selectedTarget: string | undefined;
    const currentTarget = (): string | undefined => selectedTarget ?? environmentTarget;
    const lifecycle = new OpenCodeLifecycle({
      cli,
      target: currentTarget,
      ...(options.lifecycleTimeoutMs === undefined ? {} : { timeoutMs: options.lifecycleTimeoutMs }),
    });
    const awareness = new PhuxContextAwareness(cli, {
      enabled: options.contextAwareness ?? contextAwarenessEnabled(environment.PHUX_CONTEXT_AWARENESS),
      ...(options.contextTimeoutMs === undefined ? {} : { timeoutMs: options.contextTimeoutMs }),
    });
    const latestContext = new Map<string, string>();
    const contextIdentity = () => {
      const self = normalizeTerminalIdentity(environment.PHUX_TERMINAL_ID);
      const selected = currentTarget();
      return {
        ...(self === null ? {} : { self }),
        ...(selected === undefined ? {} : { selected }),
      };
    };

    const tools = createGuardedTools(
      {
        cli,
        ...(environmentTarget === undefined ? {} : { environmentTarget }),
        getSelectedTarget: () => selectedTarget,
        selectTarget: (target) => {
          selectedTarget = target;
        },
        targetSelected: (toolContext) => {
          void lifecycle.targetSelected(toolContext.sessionID);
        },
      },
      parent,
    );

    await ctx.tool.transform((editor) => {
      for (const tool of Object.values(tools)) {
        editor.add({
          name: tool.name,
          description: tool.description,
          input: tool.input,
          options: { codemode: true },
          execute: async (input, context) => {
            const result = await tool.execute(input, toolContext(context));
            return { content: result.content, metadata: result.metadata };
          },
        });
      }
    });

    await ctx.session.hook("context", async (event) => {
      event.system.push({ type: "text", text: PARENT_PANE_RULE });
      const emission = await awareness.next(event.sessionID, contextIdentity());
      if (emission !== null) latestContext.set(event.sessionID, emission.text);
      const text = latestContext.get(event.sessionID);
      if (text !== undefined) event.system.push({ type: "text", text });
    });

    await ctx.tool.hook("execute.before", async (event) => {
      await lifecycle.toolStart(event.sessionID, event.tool, event.id);
    });
    await ctx.tool.hook("execute.after", async (event) => {
      await lifecycle.toolEnd(event.sessionID, event.tool, event.id);
    });

    const controller = new AbortController();
    const events = (async () => {
      for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
        const mapped = toLifecycleEvent(event);
        if (mapped !== undefined) await handleLifecycleEvent(lifecycle, mapped);
        const deleted = deletedSessionId(event);
        if (deleted !== undefined) {
          awareness.delete(deleted);
          latestContext.delete(deleted);
        }
      }
    })();
    void events.catch((error: unknown) => {
      if (controller.signal.aborted) return;
      console.error("phux plugin event stream ended", error);
    });

    return async () => {
      controller.abort();
      await events.catch(() => undefined);
      await lifecycle.dispose();
    };
  },
});

function toolContext(context: { readonly sessionID: string; readonly messageID: string; readonly agent: string; readonly id?: string }): ToolContext {
  return {
    sessionID: context.sessionID,
    messageID: context.messageID,
    agent: context.agent,
    id: context.id ?? context.messageID,
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
