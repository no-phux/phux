import { createStore, reconcile } from "solid-js/store";
import type { SessionRef, SetupContext } from "@opencode/plugin/gui";

import type { Overview, Pane } from "../rpc.js";
import { errorMessage, type PhuxClient } from "./client.js";
import type { definition } from "./definition.js";
import { shortPath } from "./terminal.js";

type Ctx = SetupContext<typeof definition>;

const VISIBLE_INTERVAL_MS = 2000;
const HIDDEN_INTERVAL_MS = 10_000;

export interface FleetState {
  readonly loaded: boolean;
  readonly available: boolean;
  readonly error?: string;
  readonly panes: readonly Pane[];
  readonly owned: Readonly<Record<string, readonly string[]>>;
  readonly maxTerminals: number;
}

export type Fleet = ReturnType<typeof createFleet>;

export function paneName(pane: Pane): string {
  const title = pane.title?.trim();
  if (title !== undefined && title.length > 0) return title;
  if (pane.agent.kind !== "unknown") return pane.agent.label;
  if (pane.cwd === null) return pane.target;
  const path = shortPath(pane.cwd);
  return path === "~" ? path : (path.split("/").filter(Boolean).pop() ?? pane.target);
}

/** The state a person reads: a plain shell has no agent lifecycle to report. */
export function stateKey(pane: Pane): string {
  return !isAgent(pane) && pane.state === "unknown" ? "state.shell" : `state.${pane.state}`;
}

export function isAgent(pane: Pane): boolean {
  return pane.agent.kind !== "unknown" || pane.provider !== null;
}

/** True when the pane is an OpenCode TUI showing this very session. */
export function showsSession(pane: Pane, sessionID: string): boolean {
  return pane.nativeId !== null && (pane.nativeId === sessionID || pane.nativeId === `opencode:${sessionID}`);
}

export function paneKey(ctx: Pick<Ctx, "id">, target: string): string {
  return `${ctx.id}:pane:${target}`;
}

export function fleetKey(ctx: Pick<Ctx, "id">): string {
  return `${ctx.id}:fleet`;
}

/**
 * This window's view of the phux server: polled while the window shows,
 * slower while hidden, and refreshed at once after an action. The server
 * coalesces polls from every window into one CLI call.
 */
export function createFleet(ctx: Ctx, client: PhuxClient) {
  const [state, setState] = createStore<FleetState>({ loaded: false, available: true, panes: [], owned: {}, maxTerminals: 4 });
  let previous: Map<string, Pane> | undefined;
  let previousOwned: Record<string, readonly string[]> | undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let inflight: Promise<void> | undefined;
  let stopped = false;

  const sessions = (): SessionRef[] => ctx.sessions.list().filter((session) => !session.pending);

  const apply = (overview: Overview): void => {
    const panes = [...overview.panes].sort(comparePanes);
    setState(reconcile({
      loaded: true,
      available: overview.available,
      ...(overview.error === undefined ? {} : { error: overview.error }),
      panes,
      owned: overview.owned,
      maxTerminals: overview.maxTerminals,
    }, { key: "target", merge: true }));
    announce(panes);
    openSpawned(overview.owned);
    previous = new Map(panes.map((pane) => [pane.target, pane]));
    previousOwned = { ...previousOwned, ...overview.owned };
  };

  const announce = (panes: readonly Pane[]): void => {
    if (previous === undefined) return;
    const open = new Set(ctx.sessions.list().map((session) => session.id));
    const prefs = ctx.stores.prefs.value;
    for (const pane of panes) {
      const before = previous.get(pane.target);
      if (before === undefined || before.seq === pane.seq) continue;
      // The app already announces its own sessions, wherever they show.
      if ([...open].some((id) => showsSession(pane, id))) continue;
      if (pane.state === "blocked" && before.state !== "blocked" && prefs.notifyBlocked) {
        void notify(pane, "notify.blocked", "error");
      } else if (before.state === "working" && (pane.state === "idle" || pane.state === "done") && prefs.notifyFinished) {
        void notify(pane, "notify.finished", "default");
      }
    }
  };

  const notify = async (pane: Pane, key: string, variant: "default" | "error"): Promise<void> => {
    const session = ctx.sessions.current();
    const title = ctx.t(key, { agent: paneName(pane) });
    const message = `${pane.target} · ${pane.session}${pane.cwd === null ? "" : ` · ${pane.cwd}`}`;
    const result = await ctx.attention.notify({
      title,
      message,
      variant,
      id: `phux:${pane.target}:${String(pane.seq)}`,
      ...(session === undefined ? {} : { session: { server: session.server.id, id: session.id } }),
    });
    // A focused window gets no system notification; tell it in place.
    if (!result.notification && typeof document !== "undefined" && document.hasFocus()) {
      ctx.toast.show({
        title,
        description: message,
        ...(variant === "error" ? { variant: "error" as const } : {}),
        icon: "terminal",
        actions: [{ label: ctx.t("notify.open"), run: () => openPane(pane.target) }],
      });
    }
  };

  const openSpawned = (owned: Readonly<Record<string, readonly string[]>>): void => {
    const before = previousOwned;
    if (before === undefined || !ctx.stores.prefs.value.openSpawned) return;
    for (const session of sessions()) {
      const known = before[session.id];
      if (known === undefined) continue;
      for (const target of owned[session.id] ?? []) {
        if (!known.includes(target)) ctx.layout.open(paneKey(ctx, target), session, { tab: "select", background: true });
      }
    }
  };

  /** Open a pane beside a session: the given one, the routed one, else the first open session tab. */
  const openPane = (target: string, session: SessionRef | undefined = ctx.sessions.current()): void => {
    const into = session ?? sessions()[0];
    if (into === undefined) {
      ctx.toast.show({ title: ctx.t("no.session") });
      return;
    }
    if (session === undefined) ctx.tabs.focus(into.tab);
    ctx.layout.open(paneKey(ctx, target), into);
  };

  const poll = (): Promise<void> => {
    if (inflight !== undefined) return inflight;
    const current = ctx.sessions.current();
    inflight = client
      .overview(
        { sessionIDs: sessions().map((session) => session.id) },
        current === undefined ? {} : { directory: current.directory },
      )
      .then(apply, (error: unknown) => {
        setState({ loaded: true, available: false, error: errorMessage(error) });
      })
      .finally(() => {
        inflight = undefined;
      });
    return inflight;
  };

  const schedule = (): void => {
    if (stopped) return;
    clearTimeout(timer);
    const hidden = typeof document !== "undefined" && document.visibilityState === "hidden";
    timer = setTimeout(() => void poll().finally(schedule), hidden ? HIDDEN_INTERVAL_MS : VISIBLE_INTERVAL_MS);
  };

  const refresh = (): Promise<void> => {
    const done = poll();
    schedule();
    return done;
  };

  const onVisibility = (): void => {
    if (document.visibilityState === "visible") void refresh();
  };

  void refresh();
  if (typeof document !== "undefined") document.addEventListener("visibilitychange", onVisibility);

  return {
    state,
    refresh,
    openPane,
    pane: (target: string): Pane | undefined => state.panes.find((pane) => pane.target === target),
    owned: (sessionID: string): readonly string[] => state.owned[sessionID] ?? [],
    blocked: (): Pane[] => state.panes.filter((pane) => pane.state === "blocked"),
    working: (): Pane[] => state.panes.filter((pane) => pane.state === "working"),
    stop: (): void => {
      stopped = true;
      clearTimeout(timer);
      if (typeof document !== "undefined") document.removeEventListener("visibilitychange", onVisibility);
    },
  };
}

const STATE_ORDER: Readonly<Record<Pane["state"], number>> = { blocked: 0, working: 1, done: 2, idle: 3, unknown: 4 };

function comparePanes(left: Pane, right: Pane): number {
  return STATE_ORDER[left.state] - STATE_ORDER[right.state] ||
    left.session.localeCompare(right.session) ||
    paneNumber(left.target) - paneNumber(right.target);
}

function paneNumber(target: string): number {
  const match = /@(\d+)$/.exec(target);
  return match === null ? 0 : Number(match[1]);
}
