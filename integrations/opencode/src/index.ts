import type { Plugin, PluginInput, PluginOptions } from "@opencode-ai/plugin";

import { PhuxCli, type PhuxCliOptions } from "../../pi/src/adapter.js";
import {
  PhuxContextAwareness,
  contextAwarenessEnabled,
  normalizeTerminalIdentity,
} from "../../pi/src/awareness.js";
import { handleLifecycleEvent, OpenCodeLifecycle } from "./lifecycle.js";
import { createPhuxTools } from "./tools.js";

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
export type { PhuxToolMetadata, PhuxToolRuntime } from "./tools.js";
export { handleLifecycleEvent, OpenCodeLifecycle } from "./lifecycle.js";
export type {
  OpenCodeLifecycleAdapter,
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

/**
 * Public OpenCode plugin entrypoint. Each invocation owns one selected target;
 * phux_create updates it without changing phux's global focus.
 */
export const PhuxPlugin: Plugin = async (
  _input: PluginInput,
  rawOptions?: PluginOptions,
) => {
  const options = (rawOptions ?? {}) as PhuxOpenCodeOptions;
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
    targetSelected: (context) => {
      void lifecycle.targetSelected(context.sessionID);
    },
  });

  return {
    tool: tools,
    "chat.message": async (input, output) => {
      const emission = await awareness.next(input.sessionID, contextIdentity());
      if (emission === null) return;
      output.parts.push({
        id: `phux-context-${output.message.id}-${String(emission.seq)}`,
        sessionID: output.message.sessionID,
        messageID: output.message.id,
        type: "text",
        text: emission.text,
        synthetic: true,
        metadata: {
          phuxContext: { version: emission.version, kind: emission.kind, seq: emission.seq },
        },
      });
    },
    "experimental.session.compacting": async (input, output) => {
      const emission = await awareness.checkpoint(input.sessionID, contextIdentity());
      if (emission === null) return;
      output.context.push([
        "Preserve this canonical phux fleet checkpoint in the compacted context; later phux-context sequences supersede it.",
        emission.text,
      ].join("\n"));
    },
    event: async ({ event }) => {
      await handleLifecycleEvent(lifecycle, event);
      if (event.type === "session.compacted") awareness.forceCheckpoint(event.properties.sessionID);
      if (event.type === "session.deleted") awareness.delete(event.properties.info.id);
    },
    dispose: async () => lifecycle.dispose(),
  };
};

const pluginContract: Plugin = PhuxPlugin;
void pluginContract;

export default PhuxPlugin;

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
