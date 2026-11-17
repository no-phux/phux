import type { Plugin } from "@opencode/plugin";

export interface PhuxOpenCodeV2Options {
  readonly executable?: string;
  readonly socket?: string;
  readonly lifecycleTimeoutMs?: number;
  readonly contextAwareness?: boolean;
  readonly contextTimeoutMs?: number;
}

/** 2.2+ loads `setup`; OpenCode 1.x loads `server`. */
declare const plugin: Plugin.Plugin & {
  readonly server: (input: unknown, options?: unknown) => Promise<object>;
};
export default plugin;
