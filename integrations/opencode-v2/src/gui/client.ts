import type { Context } from "@opencode/plugin/gui";

import {
  PhuxRpc,
  type InputInput,
  type KillInput,
  type Overview,
  type OverviewInput,
  type PromptInput,
  type Screen,
  type ScreenInput,
  type SpawnInput,
} from "../rpc.js";

export interface CallOptions {
  /** Route the call to the plugin instance of this workspace directory. */
  readonly directory?: string;
  readonly signal?: AbortSignal;
}

export type PhuxClient = ReturnType<typeof phuxClient>;

/**
 * Typed calls to the server plugin's RPC. Reads `ctx.client` on every call:
 * it changes when the app's server restarts.
 */
export function phuxClient(ctx: Pick<Context, "client">) {
  const call = (options: CallOptions = {}) => ({
    ...(options.directory === undefined ? {} : { location: { directory: options.directory } }),
    ...(options.signal === undefined ? {} : { signal: options.signal }),
  });
  const rpc = () => ctx.client.rpc(PhuxRpc);
  return {
    overview: (input: OverviewInput, options?: CallOptions) => rpc().overview(input, call(options)) as Promise<Overview>,
    screen: (input: ScreenInput, options?: CallOptions) => rpc().screen(input, call(options)) as Promise<Screen>,
    input: (input: InputInput, options?: CallOptions) => rpc().input(input, call(options)) as Promise<unknown>,
    prompt: (input: PromptInput, options?: CallOptions) => rpc().prompt(input, call(options)) as Promise<unknown>,
    spawn: (input: SpawnInput, options?: CallOptions) => rpc().spawn(input, call(options)) as Promise<{ target: string }>,
    kill: (input: KillInput, options?: CallOptions) => rpc().kill(input, call(options)) as Promise<unknown>,
  };
}

/** A readable message from an RPC rejection, whatever shape it takes. */
export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (error !== null && typeof error === "object" && typeof (error as { message?: unknown }).message === "string") {
    return (error as { message: string }).message;
  }
  return String(error);
}
