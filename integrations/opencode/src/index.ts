import {
  tool,
  type Plugin,
  type PluginOptions,
  type ToolContext as OpenCodeToolContext,
  type ToolDefinition as OpenCodeToolDefinition,
} from "@opencode-ai/plugin";

import { PhuxCli, type PhuxCliOptions } from "../../pi/src/adapter.js";
import {
  PhuxContextAwareness,
  contextAwarenessEnabled,
  normalizeTerminalIdentity,
} from "../../pi/src/awareness.js";
import {
  handleLifecycleEvent,
  OpenCodeLifecycle,
  type OpenCodeLifecycleEvent,
} from "./lifecycle.js";
import { createPhuxTools, type PhuxToolDefinition, type ToolContext } from "./tools.js";

export { PhuxCli } from "../../pi/src/adapter.js";
export {
  PhuxContextAwareness,
  contextAwarenessEnabled,
  normalizeTerminalIdentity,
} from "../../pi/src/awareness.js";
export type {
  PhuxContextAwarenessOptions,
  PhuxContextEmission,
  PhuxContextIdentity,
} from "../../pi/src/awareness.js";
export type {
  AgentTargetOptions,
  CreateOptions,
  ExecutionOptions,
  PhuxCliOptions,
  PhuxProbe,
  RunOptions,
  SnapshotOptions,
  WaitOptions,
  WaitOutcome,
} from "../../pi/src/adapter.js";
export {
  boundedResult,
  createPhuxTools,
  DEFAULT_SHORT_TIMEOUT_MS,
  MAX_MODEL_BYTES,
  MAX_MODEL_LINES,
  resolveTarget,
} from "./tools.js";
export type {
  PhuxToolDefinition,
  PhuxToolMetadata,
  PhuxToolRuntime,
  ToolContext,
  ToolResult,
} from "./tools.js";
export { handleLifecycleEvent, OpenCodeLifecycle } from "./lifecycle.js";
export type {
  OpenCodeLifecycleAdapter,
  OpenCodeLifecycleEvent,
  OpenCodeLifecycleOptions,
  OpenCodeLifecycleState,
} from "./lifecycle.js";

/** Plugin settings plus injectable seams for library and contract tests. */
export interface PhuxOpenCodeOptions {
  readonly executable?: string;
  readonly socket?: string;
  readonly lifecycleTimeoutMs?: number;
  readonly contextAwareness?: boolean;
  readonly contextTimeoutMs?: number;
  readonly cli?: PhuxCli;
  readonly env?: NodeJS.ProcessEnv;
  readonly onLifecycleError?: (error: unknown) => void;
}

/** Build an OpenCode plugin with optional test-only defaults. */
export function createPhuxPlugin(defaults: PhuxOpenCodeOptions = {}): Plugin {
  return async (_input, configured) => {
    const options = mergeOptions(defaults, configured ?? {});
    const environment = options.env ?? process.env;
    const environmentTarget = readEnvironmentTarget(environment.PHUX_TARGET);
    const cli = options.cli ?? new PhuxCli(cliOptions(options, environment));
    let selectedTarget: string | undefined;
    const currentTarget = (): string | undefined => selectedTarget ?? environmentTarget;
    const lifecycle = new OpenCodeLifecycle({
      cli,
      target: currentTarget,
      ...(options.lifecycleTimeoutMs === undefined ? {} : { timeoutMs: options.lifecycleTimeoutMs }),
      ...(options.onLifecycleError === undefined ? {} : { onError: options.onLifecycleError }),
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

    const tools = createPhuxTools({
      cli,
      ...(environmentTarget === undefined ? {} : { environmentTarget }),
      getSelectedTarget: () => selectedTarget,
      selectTarget: (target) => {
        selectedTarget = target;
      },
      targetSelected: (toolContext) => {
        void lifecycle.targetSelected(toolContext.sessionID);
      },
    });

    return {
      tool: openCodeTools(tools),
      "experimental.chat.system.transform": async (input, output) => {
        if (input.sessionID === undefined) return;
        const emission = await awareness.next(input.sessionID, contextIdentity());
        if (emission !== null) latestContext.set(input.sessionID, emission.text);
        const text = latestContext.get(input.sessionID);
        if (text === undefined) return;
        output.system.push(text);
      },
      event: async ({ event }) => {
        if (!isLifecycleEvent(event)) return;
        await handleLifecycleEvent(lifecycle, event);
        if (event.type !== "session.deleted") return;
        const info = event.properties.info;
        const sessionID = info !== null && typeof info === "object" ? (info as { readonly id?: unknown }).id : undefined;
        if (typeof sessionID !== "string") return;
        awareness.delete(sessionID);
        latestContext.delete(sessionID);
      },
      dispose: async () => lifecycle.dispose(),
    };
  };
}

export const PhuxPlugin = createPhuxPlugin();
export default PhuxPlugin;

const z = tool.schema;
const localTimeoutArgument = () => z.number().int().min(1).max(3_600_000).optional();
const targetArgument = () => nonBlankString(512).optional();

function nonBlankString(maxLength: number) {
  return z.string().min(1).max(maxLength).regex(/\S/);
}

function openCodeTools(tools: Record<string, PhuxToolDefinition<any>>): Record<string, OpenCodeToolDefinition> {
  return {
    phux_list: adaptTool(tools.phux_list!, { local_timeout_ms: localTimeoutArgument() }),
    phux_create: adaptTool(tools.phux_create!, {
      name: nonBlankString(255),
      cwd: nonBlankString(4096).optional(),
      command: z.array(z.string().max(65_536)).min(1).max(256).optional(),
      local_timeout_ms: localTimeoutArgument(),
    }),
    phux_snapshot: adaptTool(tools.phux_snapshot!, {
      target: targetArgument(),
      scrollback: z.number().int().min(0).max(100_000).optional(),
      cells: z.boolean().optional(),
      local_timeout_ms: localTimeoutArgument(),
    }),
    phux_send_keys: adaptTool(tools.phux_send_keys!, {
      target: targetArgument(),
      keys: z.array(nonBlankString(65_536)).min(1).max(256),
      local_timeout_ms: localTimeoutArgument(),
    }),
    phux_run: adaptTool(tools.phux_run!, {
      target: targetArgument(),
      command: nonBlankString(65_536),
      timeout_seconds: z.number().int().min(0).max(86_400).optional(),
      local_timeout_ms: localTimeoutArgument(),
    }),
    phux_wait: adaptTool(tools.phux_wait!, {
      target: targetArgument(),
      until: nonBlankString(4096).optional(),
      idle_ms: z.number().int().min(0).max(86_400_000).optional(),
      timeout_seconds: z.number().int().min(1).max(86_400).optional(),
      local_timeout_ms: localTimeoutArgument(),
    }),
  };
}

function adaptTool(
  definition: PhuxToolDefinition<any>,
  args: Parameters<typeof tool>[0]["args"],
): OpenCodeToolDefinition {
  return tool({
    description: definition.description,
    args,
    execute: async (input, context) => {
      const result = await definition.execute(input, phuxToolContext(context));
      return { output: result.content, metadata: result.metadata };
    },
  });
}

function phuxToolContext(context: OpenCodeToolContext): ToolContext {
  return {
    sessionID: context.sessionID,
    messageID: context.messageID,
    agent: context.agent,
    id: context.messageID,
  };
}

function isLifecycleEvent(value: unknown): value is OpenCodeLifecycleEvent {
  if (value === null || typeof value !== "object") return false;
  const candidate = value as { readonly type?: unknown; readonly properties?: unknown };
  return typeof candidate.type === "string" && candidate.properties !== null && typeof candidate.properties === "object";
}

function mergeOptions(defaults: PhuxOpenCodeOptions, configured: PluginOptions): PhuxOpenCodeOptions {
  return { ...defaults, ...(configured as PhuxOpenCodeOptions) };
}

function cliOptions(options: PhuxOpenCodeOptions, environment: NodeJS.ProcessEnv): PhuxCliOptions {
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
