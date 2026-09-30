import type { Plugin } from "@opencode/plugin";

export interface PhuxOpenCodeV2Options {
  readonly executable?: string;
  readonly socket?: string;
  readonly lifecycleTimeoutMs?: number;
  readonly contextAwareness?: boolean;
  readonly contextTimeoutMs?: number;
}

declare const plugin: Plugin.Plugin;
export default plugin;
