import type { PhuxCli } from "../../runtime/src/adapter.js";
import { PhuxError } from "../../runtime/src/errors.js";
import type { AgentPane } from "../../runtime/src/schemas.js";
import type { PhuxScreen } from "../../runtime/src/screen.js";
import type {
  InputInput,
  KillInput,
  Overview,
  OverviewInput,
  Pane,
  PromptInput,
  Screen,
  ScreenInput,
  SpawnInput,
} from "./rpc.js";
import { TARGET_PATTERN } from "./rpc.js";
import type { OwnedTerminals } from "./terminals.js";

type GuiCli = Pick<PhuxCli, "executable" | "socket" | "agentList" | "snapshot" | "paste" | "sendKeys" | "agentPrompt" | "spawn">;

export interface GuiServiceOptions {
  readonly cli: GuiCli;
  readonly screen: Pick<PhuxScreen, "rendered" | "watchUntil">;
  readonly terminals: OwnedTerminals;
  readonly maxTerminals: number;
  /** The pane hosting this server, which never receives input from the GUI. */
  readonly parent: string | null;
  readonly selectedTargets: Map<string, string>;
}

export interface CallContext {
  readonly signal?: AbortSignal;
}

/**
 * Events that make a watched pane worth repainting: `dirty` fires when output
 * starts and `idle` when it settles. `unknown` also matches `journal_gap` and
 * `source_gap`, so a cursor the server can no longer replay wakes at once.
 */
const SCREEN_EVENTS = ["dirty", "idle", "title_changed", "pane_closed", "unknown"] as const;
const DEFAULT_HISTORY = 500;
const TARGET = new RegExp(TARGET_PATTERN);
const OVERVIEW_TTL_MS = 1000;

/**
 * Every plugin instance (one per OpenCode location) registers its terminals,
 * so a window asking about a session finds them whichever location it asks.
 */
const instances = new Set<OwnedTerminals>();
const overviews = new Map<string, { readonly at: number; readonly result: Promise<Omit<Overview, "owned" | "maxTerminals">> }>();
const observed = new Map<string, Map<string, { state: string; seq: number; since: number }>>();

export function registerTerminals(terminals: OwnedTerminals): () => void {
  instances.add(terminals);
  return () => instances.delete(terminals);
}

export function ownedBy(sessionID: string): string[] {
  const owned = new Set<string>();
  for (const terminals of instances) for (const target of terminals.owned(sessionID)) owned.add(target);
  return [...owned];
}

export function createGuiService(options: GuiServiceOptions) {
  const { cli } = options;
  const cacheKey = JSON.stringify([cli.executable, cli.socket ?? null]);

  const parent = options.parent === null ? null : canonicalTarget(options.parent);
  /** A pane the GUI may type into: exact, and never the pane hosting this server, however it is spelled. */
  const writable = (target: string): string => {
    const canonical = canonicalTarget(target);
    if (canonical === parent) throw new Error(`${target} hosts this OpenCode server; phux does not type into its own pane.`);
    return canonical;
  };

  const fleet = (): Promise<Omit<Overview, "owned" | "maxTerminals">> => {
    const cached = overviews.get(cacheKey);
    if (cached !== undefined && Date.now() - cached.at < OVERVIEW_TTL_MS) return cached.result;
    // One CLI call serves every window and location polling within the TTL.
    const result = cli.agentList({ timeoutMs: 10_000 }).then(
      (list) => ({ available: true, panes: stamp(cacheKey, list.agents.map(toPane)) }),
      (error: unknown) => ({ available: false, error: describe(error), panes: [] }),
    );
    overviews.set(cacheKey, { at: Date.now(), result });
    return result;
  };
  const invalidate = (): void => {
    overviews.delete(cacheKey);
  };

  return {
    async overview(input: OverviewInput): Promise<Overview> {
      const result = await fleet();
      return {
        ...result,
        owned: Object.fromEntries((input.sessionIDs ?? []).map((sessionID) => [sessionID, ownedBy(sessionID)])),
        maxTerminals: options.maxTerminals,
      };
    },

    /**
     * A long poll: wait until the pane changes after `after` (or the wait
     * ends), then render it. A render identical to the caller's `known` hash
     * comes back without content, so a quiet pane costs one small reply and a
     * change that raced the watch still shows by the next wait.
     */
    async screen(request: ScreenInput, context: CallContext = {}): Promise<Screen> {
      const input = { ...request, target: canonicalTarget(request.target) };
      const signal = context.signal === undefined ? {} : { signal: context.signal };
      let cursor = input.after;
      let event: string | undefined;
      if ((input.waitSeconds ?? 0) > 0) {
        try {
          const watched = await options.screen.watchUntil(input.target, {
            until: SCREEN_EVENTS,
            timeoutSeconds: input.waitSeconds!,
            ...(input.after === undefined ? {} : { after: input.after }),
            ...signal,
          });
          cursor = watched.cursor ?? cursor;
          event = watched.event;
          if (event === "pane_closed") return { changed: true, closed: true, ...(cursor === undefined ? {} : { cursor }) };
        } catch (error) {
          if (isGone(error)) return { changed: true, closed: true };
          if (!isUsageError(error)) throw error;
          // A phux without resumable watches still renders; the caller's wait becomes a poll.
          await sleep(Math.min(input.waitSeconds!, 2) * 1000, context.signal);
        }
      }
      const withCursor = { ...(cursor === undefined ? {} : { cursor }), ...(event === undefined ? {} : { event }) };
      const history = input.history ?? DEFAULT_HISTORY;
      const execution = { timeoutMs: 10_000, ...signal };
      // The Formatter trims trailing blank rows, so the plain read places the
      // viewport: its history count is where the caret's row starts.
      const [rendered, plain] = await Promise.allSettled([
        options.screen.rendered(input.target, { history, ...execution }),
        cli.snapshot({ target: input.target, scrollback: history, ...execution }),
      ]);
      if ([rendered, plain].some((result) => result.status === "rejected" && isGone(result.reason))) {
        return { changed: true, closed: true, ...withCursor };
      }
      if (plain.status === "rejected") throw plain.reason;
      if (rendered.status === "rejected" && !isUsageError(rendered.reason)) throw rendered.reason;
      const screen = plain.value;
      // A phux without the html Formatter still reads plain rows.
      const content = rendered.status === "fulfilled"
        ? { html: rendered.value.html, title: rendered.value.title }
        : { lines: [...screen.scrollback, ...screen.lines] };
      const geometry = { cols: screen.cols, rows: screen.rows, historyRows: screen.scrollback.length, caret: screen.cursor };
      const hash = fingerprint(JSON.stringify([content, geometry]));
      if (hash === input.known) return { changed: false, closed: false, ...withCursor, hash };
      return { changed: true, closed: false, ...withCursor, hash, ...geometry, ...content };
    },

    async input(input: InputInput, context: CallContext = {}): Promise<{ ok: true }> {
      const target = writable(input.target);
      if (input.text === undefined && input.keys === undefined && input.submit !== true) {
        throw new TypeError("input needs text, keys, or submit");
      }
      const execution = { timeoutMs: 10_000, ...(context.signal === undefined ? {} : { signal: context.signal }) };
      if (input.text !== undefined) await cli.paste(target, input.text, execution);
      if (input.keys !== undefined) await cli.sendKeys(target, input.keys.flatMap(keyArgument), execution);
      if (input.submit === true) await cli.sendKeys(target, ["Enter"], execution);
      return { ok: true };
    },

    /** A turn handed to an agent pane, with phux's delivery receipt. */
    async prompt(input: PromptInput, context: CallContext = {}): Promise<{ ok: true }> {
      const target = writable(input.target);
      const execution = { timeoutMs: 15_000, ...(context.signal === undefined ? {} : { signal: context.signal }) };
      if (/[\r\n]/.test(input.text)) {
        await cli.paste(target, input.text, execution);
        await cli.sendKeys(target, ["Enter"], execution);
      } else {
        await cli.agentPrompt(target, input.text, { wait: false, ...execution });
      }
      invalidate();
      return { ok: true };
    },

    /** Spawn a terminal owned by the session, exactly as the agent's phux_spawn does. */
    async spawn(input: SpawnInput, context: CallContext = {}): Promise<{ target: string }> {
      const signal = context.signal === undefined ? {} : { signal: context.signal };
      await options.terminals.admit(input.sessionID, context.signal);
      const result = await cli.spawn({
        timeoutMs: 15_000,
        ...(input.cwd === undefined ? {} : { cwd: input.cwd }),
        ...(input.command === undefined ? {} : { command: input.command }),
        ...signal,
      });
      const target = result.satellite === null ? `@${result.terminal_id}` : `${result.satellite}/@${result.terminal_id}`;
      await options.terminals.add(input.sessionID, target);
      options.selectedTargets.set(input.sessionID, target);
      invalidate();
      return { target };
    },

    /** Close a terminal the session owns, from whichever location spawned it. */
    async kill(input: KillInput, context: CallContext = {}): Promise<{ ok: true }> {
      const target = canonicalTarget(input.target);
      const owner = [...instances].find((terminals) => terminals.owned(input.sessionID).includes(target)) ?? options.terminals;
      await owner.kill(input.sessionID, target, context.signal);
      if (options.selectedTargets.get(input.sessionID) === target) options.selectedTargets.delete(input.sessionID);
      invalidate();
      return { ok: true };
    },
  };
}

export type GuiService = ReturnType<typeof createGuiService>;

/** Number each observed state change once, server-wide, and forget panes that left. */
function stamp(key: string, panes: readonly Omit<Pane, "seq" | "since">[], now = Date.now()): Pane[] {
  const previous = observed.get(key) ?? new Map<string, { state: string; seq: number; since: number }>();
  const next = new Map<string, { state: string; seq: number; since: number }>();
  const result = panes.map((pane) => {
    const before = previous.get(pane.target);
    const entry = before === undefined
      ? { state: pane.state, seq: 0, since: now }
      : before.state === pane.state ? before : { state: pane.state, seq: before.seq + 1, since: now };
    next.set(pane.target, entry);
    return { ...pane, seq: entry.seq, since: entry.since };
  });
  observed.set(key, next);
  return result;
}

export function toPane(agent: AgentPane): Omit<Pane, "seq" | "since"> {
  return {
    target: agent.terminal,
    session: agent.session,
    title: agent.title,
    cwd: agent.cwd,
    agent: { id: agent.agent.id, label: agent.agent.label, kind: agent.agent.kind },
    state: agent.state,
    attention: agent.attention,
    provider: agent.agent_session?.provider ?? null,
    nativeId: agent.agent_session?.native_id ?? null,
  };
}

/** FNV-1a, enough to tell two renders of one pane apart. */
function fingerprint(text: string): string {
  let hash = 0x811c9dc5;
  for (let index = 0; index < text.length; index++) {
    hash ^= text.charCodeAt(index);
    hash = Math.imul(hash, 0x01000193);
  }
  return `${(hash >>> 0).toString(36)}:${text.length.toString(36)}`;
}

/**
 * Only exact pane selectors, never a session, tag, or agent name that could
 * fan out; `@07` and `@7` are one pane, so compare the canonical spelling.
 */
export function canonicalTarget(target: string): string {
  if (!TARGET.test(target)) throw new TypeError(`${JSON.stringify(target)} is not an exact @N or host/@N pane selector`);
  return target.replace(/@0+(?=\d)/, "@");
}

/**
 * send-keys reads its keys after the target with flags still live, so a key
 * that looks like a flag (`-h`, `--`, `--socket=…`) is typed one character at
 * a time; a lone `-` is an ordinary value.
 */
export function keyArgument(key: string): string[] {
  return key.length > 1 && key.startsWith("-") ? [...key] : [key];
}

function sleep(ms: number, signal: AbortSignal | undefined): Promise<void> {
  return new Promise((resolve) => {
    if (signal?.aborted === true) return resolve();
    const done = (): void => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", done);
      resolve();
    };
    const timer = setTimeout(done, ms);
    signal?.addEventListener("abort", done, { once: true });
  });
}

function cliErrorCode(error: unknown): string | undefined {
  if (!(error instanceof PhuxError)) return undefined;
  const detail = error.cliError?.["error"];
  if (detail === null || typeof detail !== "object" || Array.isArray(detail)) return undefined;
  const code = (detail as Record<string, unknown>).code;
  return typeof code === "string" ? code : undefined;
}

function isGone(error: unknown): boolean {
  if (cliErrorCode(error) === "no_such_target") return true;
  // A missing phux executable is not a closed pane.
  if (!(error instanceof PhuxError) || error.code !== "command_failed") return false;
  const message = error instanceof Error ? error.message : String(error);
  return /no such target|not found|does not exist|unknown (pane|terminal)/i.test(message);
}

function isUsageError(error: unknown): boolean {
  return cliErrorCode(error) === "usage_error" || (error instanceof PhuxError && error.exitCode === 2);
}

function describe(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  return message.length > 600 ? `${message.slice(0, 600)}…` : message;
}
