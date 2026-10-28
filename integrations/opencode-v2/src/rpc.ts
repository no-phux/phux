import { Rpc } from "@opencode/plugin/rpc";

/**
 * The contract between the server plugin, which owns the phux CLI and each
 * session's terminals, and the desktop and web GUI. Outputs are documented by
 * the TypeScript types below; the schemas validate what crosses the wire.
 */

export type PaneState = "unknown" | "idle" | "working" | "blocked" | "done";
export type PaneAttention = "none" | "low" | "normal" | "high";

export interface Pane {
  readonly target: string;
  readonly session: string;
  readonly title: string | null;
  readonly cwd: string | null;
  readonly agent: { readonly id: string; readonly label: string; readonly kind: string };
  readonly state: PaneState;
  readonly attention: PaneAttention;
  /** The agent session this pane hosts, when phux knows it, such as an OpenCode session id. */
  readonly provider: string | null;
  readonly nativeId: string | null;
  /** Bumps each time the server observes a new state, so every window agrees on one transition. */
  readonly seq: number;
  /** When the server first observed the current state, in epoch milliseconds. */
  readonly since: number;
}

export interface Overview {
  readonly available: boolean;
  readonly error?: string;
  readonly panes: readonly Pane[];
  /** Panes each requested OpenCode session spawned and owns. */
  readonly owned: Readonly<Record<string, readonly string[]>>;
  readonly maxTerminals: number;
}

export interface Caret {
  readonly x: number;
  readonly y: number;
  readonly visible: boolean;
}

export interface Screen {
  readonly changed: boolean;
  readonly closed: boolean;
  /** Resume point for the next long poll. */
  readonly cursor?: string;
  /** Identity of this render; pass it back as `known` to skip an unchanged one. */
  readonly hash?: string;
  /** Inline-styled markup from the phux server's Formatter: untrusted, never assigned as HTML. */
  readonly html?: string;
  /** Plain rows, from a phux too old to render html. */
  readonly lines?: readonly string[];
  readonly title?: string | null;
  readonly cols?: number;
  readonly rows?: number;
  /** Rows of history above the viewport; the caret's row counts from there. */
  readonly historyRows?: number;
  readonly caret?: Caret | null;
}

export const TARGET_PATTERN = "^(?:[^/\\s]+/)?@[0-9]+$";
// RPC validation refuses JSON Schema patterns; the server checks TARGET_PATTERN itself.
const TARGET = { type: "string", minLength: 2, maxLength: 512 } as const;
const SESSION = { type: "string", minLength: 1, maxLength: 256 } as const;
const OBJECT = { type: "object" } as const;
const KEY = { type: "string", minLength: 1, maxLength: 64 } as const;

export const PhuxRpc = Rpc.define({
  id: "phux",
  methods: {
    overview: {
      input: {
        type: "object",
        properties: { sessionIDs: { type: "array", maxItems: 256, items: SESSION } },
        additionalProperties: false,
      },
      output: OBJECT,
    },
    screen: {
      input: {
        type: "object",
        properties: {
          target: TARGET,
          history: { type: "integer", minimum: 0, maximum: 10_000 },
          after: { type: "string", minLength: 1, maxLength: 512 },
          waitSeconds: { type: "integer", minimum: 0, maximum: 25 },
          known: { type: "string", minLength: 1, maxLength: 64 },
        },
        required: ["target"],
        additionalProperties: false,
      },
      output: OBJECT,
    },
    input: {
      input: {
        type: "object",
        properties: {
          target: TARGET,
          text: { type: "string", minLength: 1, maxLength: 65_536 },
          keys: { type: "array", minItems: 1, maxItems: 256, items: KEY },
          submit: { type: "boolean" },
        },
        required: ["target"],
        additionalProperties: false,
      },
      output: OBJECT,
    },
    prompt: {
      input: {
        type: "object",
        properties: { target: TARGET, text: { type: "string", minLength: 1, maxLength: 65_536 } },
        required: ["target", "text"],
        additionalProperties: false,
      },
      output: OBJECT,
    },
    spawn: {
      input: {
        type: "object",
        properties: {
          sessionID: SESSION,
          cwd: { type: "string", minLength: 1, maxLength: 4096 },
          command: { type: "array", minItems: 1, maxItems: 256, items: { type: "string", maxLength: 65_536 } },
        },
        required: ["sessionID"],
        additionalProperties: false,
      },
      output: OBJECT,
    },
    kill: {
      input: {
        type: "object",
        properties: { sessionID: SESSION, target: TARGET },
        required: ["sessionID", "target"],
        additionalProperties: false,
      },
      output: OBJECT,
    },
  },
  events: {},
});

export interface OverviewInput {
  readonly sessionIDs?: readonly string[];
}

export interface ScreenInput {
  readonly target: string;
  readonly history?: number;
  readonly after?: string;
  readonly waitSeconds?: number;
  readonly known?: string;
}

export interface InputInput {
  readonly target: string;
  readonly text?: string;
  readonly keys?: readonly string[];
  readonly submit?: boolean;
}

export interface PromptInput {
  readonly target: string;
  readonly text: string;
}

export interface SpawnInput {
  readonly sessionID: string;
  readonly cwd?: string;
  readonly command?: readonly string[];
}

export interface KillInput {
  readonly sessionID: string;
  readonly target: string;
}
