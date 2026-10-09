/**
 * The typed native boundary. One `DesktopClient` per connection, one drain
 * per wake: this module owns `takeEvents`, turns the batch into Solid
 * signals, and forwards current events to one subscriber. Ambiguous and
 * pre-connection badges are omitted. UI code never parses VT or retries delivery.
 */
import { batch, createSignal, type Accessor, type Signal } from "solid-js";
import type {
  DesktopClient,
  DesktopEvent,
  DesktopPane,
  DesktopPathAnswer,
  DesktopServerInfo,
  DesktopSession,
  DesktopSpawnOptions,
  DesktopTopology,
  EventPayload,
  GlobalHotkeys,
  GpuixRenderer,
} from "../../native/generated/index";

export interface DesktopHost {
  DesktopClient: new () => DesktopClient;
  GpuixRenderer: new (
    callback: (error: Error | null, event: EventPayload) => void,
  ) => GpuixRenderer;
  GlobalHotkeys: new () => GlobalHotkeys;
}

export interface AgentInfo {
  name: string;
  kind?: string;
  state: string;
  attention: string;
  changedAt: number;
}

export interface ConnectTarget {
  socketPath: string;
  sessionName: string;
}

/** What a caller chooses for a new terminal; the bridge supplies the identity. */
export type SpawnRequest = Omit<DesktopSpawnOptions, "identity" | "sessionId"> & {
  sessionId?: number;
};

export interface Bridge {
  status: Accessor<string>;
  error: Accessor<string | undefined>;
  topology: Accessor<DesktopTopology | undefined>;
  server: Accessor<DesktopServerInfo | undefined>;
  agents: Accessor<Record<string, AgentInfo>>;
  /**
   * Bumps on every drained wake; read it to re-evaluate per-terminal native
   * state (readiness, delivery fences, paint). Topology, status and server
   * refresh only on wakes whose events can change them.
   */
  revision: Accessor<number>;
  /**
   * Bumps when a drained wake can change what this terminal paints: its own
   * output, or any event that is not terminal-scoped output or a badge. A
   * mounted terminal's `paintRevision`, so a wake that only moved terminals
   * nobody shows (a background tab's flood) redraws nothing.
   */
  paintRevision(terminalId: string): number;
  handle: Accessor<string>;
  target: ConnectTarget;
  client(): DesktopClient;
  connect(): void;
  reconnect(): void;
  close(): void;
  ready(terminalId: string): boolean;
  fenced(terminalId: string): boolean;
  homeSession(): DesktopSession | undefined;
  panes(): DesktopPane[];
  /** Ask the server for a fresh session list after a create or rename. */
  refreshTopology(): void;
  /**
   * Spawn a terminal in `sessionId` when that session is in the topology.
   * An omitted session uses the window session. A named session that is
   * absent does not fall back to another session.
   */
  spawn(options: SpawnRequest): number | undefined;
  onEvents(listener: (events: DesktopEvent[]) => void): void;
  /** Host path answers (`PATH_QUERY`), drained on the same wake as events. */
  onPathAnswers(listener: (answers: DesktopPathAnswer[]) => void): void;
}

/**
 * Wake drains across every bridge in this process, for `PHUX_DESKTOP_PERF`:
 * how many wakes, how many events they carried, the milliseconds spent
 * draining and applying them (including the Solid updates they trigger), and
 * how many wakes waited for the next frame (`DRAIN_INTERVAL_MS`).
 */
export const drainStats = { wakes: 0, events: 0, ms: 0, maxMs: 0, deferred: 0 };

/**
 * Minimum milliseconds between drains: one 60 Hz frame. A drain that changes
 * what the window shows costs a whole-window GPUIX draw, layout included,
 * and a display shows at most one per refresh. A wake after a quiet frame
 * drains at once, so an echo is not delayed; a wake inside the frame drains
 * at its end. The runtime does not wake again until a drain rearms it, so a
 * deferred wake is coalesced, never lost.
 */
export const DRAIN_INTERVAL_MS = 16;

/**
 * Whether an event can change the topology, status or server snapshot. Frame
 * damage, delivery receipts and agent badges cannot: the runtime queues a
 * topology, status or lifecycle event for every change to those (and a
 * `TopologyChanged` when it drops events on overflow).
 */
export function structural(event: DesktopEvent): boolean {
  return !(
    event.kind === "TerminalChanged" ||
    event.kind === "InputDelivery" ||
    event.kind === "AgentBadge"
  );
}

/**
 * The server's or runtime's reason when acknowledged input (a paste, an
 * inserted path) was refused, so the shell can say so instead of the paste
 * silently vanishing. Delivered and Unknown are not refusals: Unknown shows
 * as the pane's delivery fence.
 */
export function refusedInput(event: DesktopEvent): string | undefined {
  if (event.kind !== "InputDelivery" || `${event.outcome}` !== "Refused") return undefined;
  return event.message || "The terminal did not accept the input.";
}

export function startsConnection(event: DesktopEvent): boolean {
  // The binding projects both runtime Connecting and Negotiated to Connecting.
  return event.kind === "StatusChanged" && `${event.status}` === "Connecting";
}

export function createBridge(
  host: DesktopHost,
  target: ConnectTarget,
  drainInterval = DRAIN_INTERVAL_MS,
): Bridge {
  const [status, setStatus] = createSignal("Connecting");
  const [error, setError] = createSignal<string | undefined>();
  const [topology, setTopology] = createSignal<DesktopTopology | undefined>();
  const [server, setServer] = createSignal<DesktopServerInfo | undefined>();
  const [agents, setAgents] = createSignal<Record<string, AgentInfo>>({});
  const [revision, setRevision] = createSignal(0);
  // One strictly increasing tick per drain feeds every paint revision, so a
  // revision never repeats a value, even for a terminal whose signal was
  // dropped and recreated.
  let paintClock = 0;
  const [everyTerminal, setEveryTerminal] = createSignal(0);
  const terminalRevisions = new Map<string, Signal<number>>();
  const [handle, setHandle] = createSignal("");
  let owner: DesktopClient | undefined;
  let closed = false;
  const knownTerminals = new Set<string>();
  let listener: (events: DesktopEvent[]) => void = () => {};
  let pathListener: (answers: DesktopPathAnswer[]) => void = () => {};

  function terminalRevision(terminalId: string): Signal<number> {
    const known = terminalRevisions.get(terminalId);
    if (known) return known;
    const [read, write] = createSignal(0);
    const signal: Signal<number> = [read, write];
    terminalRevisions.set(terminalId, signal);
    return signal;
  }

  /** Bump only the terminals this batch can repaint; see `paintRevision`. */
  function invalidatePaint(events: DesktopEvent[]): void {
    const output = new Set<string>();
    // An empty drain is a wake for state that changed without an event (such
    // as readiness), which any terminal may paint.
    let all = events.length === 0;
    for (const event of events) {
      if (event.kind === "TerminalChanged") output.add(event.terminalId);
      else if (event.kind !== "AgentBadge") all = true;
      // Bumping every terminal re-runs whatever still reads this one, which
      // then subscribes to a fresh signal instead of the dropped one.
      if (event.kind === "Closed") terminalRevisions.delete(event.terminalId);
    }
    if (!all && output.size === 0) return;
    paintClock += 1;
    if (all) setEveryTerminal(paintClock);
    for (const terminalId of output) terminalRevision(terminalId)[1](paintClock);
  }

  function client(): DesktopClient {
    if (!owner) throw new Error("Desktop client is not connected");
    return owner;
  }

  function accept(events: DesktopEvent[], acceptBadges = true): void {
    const boundary = events.findLastIndex(startsConnection);
    const current =
      acceptBadges && boundary < 0
        ? events
        : events.filter(
            (event, index) => event.kind !== "AgentBadge" || (acceptBadges && index > boundary),
          );
    for (const event of current) {
      if (startsConnection(event)) setAgents({});
      if (event.kind === "ServerError") setError(event.message);
      if (event.kind === "AgentBadge") noteAgent(event);
      if (event.kind === "Closed") forgetAgent(event.terminalId);
    }
    listener(current);
  }

  function noteAgent(event: Extract<DesktopEvent, { kind: "AgentBadge" }>): void {
    if (!event.name) {
      forgetAgent(event.terminalId);
      return;
    }
    setAgents((current) => {
      const next = { ...current };
      const previous = current[event.terminalId];
      const changed = previous?.state !== event.state || previous?.attention !== event.attention;
      const info: AgentInfo = {
        name: event.name,
        state: event.state,
        attention: event.attention,
        changedAt: changed || !previous ? Date.now() : previous.changedAt,
      };
      if (event.agentKind) info.kind = event.agentKind;
      next[event.terminalId] = info;
      return next;
    });
  }

  function forgetAgent(terminalId: string): void {
    setAgents((current) => {
      if (!current[terminalId]) return current;
      const next = { ...current };
      delete next[terminalId];
      return next;
    });
  }

  function snapshot(): DesktopServerInfo | undefined {
    const native = client();
    const failure = native.lastError();
    const next = native.topology() ?? undefined;
    const info = native.serverInfo() ?? undefined;
    knownTerminals.clear();
    for (const pane of next?.panes ?? []) knownTerminals.add(pane.terminalId);
    // A snapshot follows a structural event, which bumps every terminal, so a
    // pane still reading a dropped id re-subscribes to a fresh signal.
    for (const terminalId of terminalRevisions.keys())
      if (!knownTerminals.has(terminalId)) terminalRevisions.delete(terminalId);
    batch(() => {
      setStatus(native.status());
      if (failure) setError(failure);
      setTopology(next);
      if (info) {
        // Overflow can discard a connection boundary, even on the same daemon.
        // Rebuild labels from this connection's replay rather than retaining old ones.
        if (
          server()?.serverId !== info.serverId ||
          server()?.connectionEpoch !== info.connectionEpoch
        )
          setAgents({});
        setServer(info);
      }
    });
    return info;
  }

  let lastDrain = Number.NEGATIVE_INFINITY;
  let deferred: ReturnType<typeof setTimeout> | undefined;

  function activity(from: string): void {
    if (closed || !owner || from !== owner.handle) return;
    const wait = lastDrain + drainInterval - performance.now();
    if (wait > 0) {
      if (deferred === undefined) {
        drainStats.deferred += 1;
        deferred = setTimeout(() => {
          deferred = undefined;
          activity(from);
        }, wait);
      }
      return;
    }
    drain();
  }

  function drain(): void {
    if (!owner) return;
    const started = performance.now();
    lastDrain = started;
    // Cache the negotiated identity on ordinary output wakes. Only a new epoch
    // needs another full native snapshot before the drain.
    const previous = server();
    const before =
      previous?.connectionEpoch === owner.connectionEpoch()
        ? previous
        : (owner.serverInfo() ?? undefined);
    // One drain per wake, even when empty: the drain rearms notification.
    const events = owner.takeEvents();
    // Drain every wake: the runtime's answer queue is bounded, and a full
    // queue refuses the next query.
    const answers = owner.takePathAnswers();
    batch(() => {
      // A new topology object re-renders every row derived from it, so read
      // one only when an event can have changed it: output, receipts and
      // agent badges cannot, and under a flood they are nearly every wake.
      const changed = events.some(structural);
      const current = changed ? snapshot() : undefined;
      // A Connecting event may itself belong to the retired connection. Only
      // stable pre-/post-drain identity can attribute badges to a replacement.
      const acceptBadges =
        !changed ||
        (!!before &&
          !!current &&
          before.serverId === current.serverId &&
          before.connectionEpoch === current.connectionEpoch);
      accept(events, acceptBadges);
      if (answers.length > 0) pathListener(answers);
      invalidatePaint(events);
      setRevision((value) => value + 1);
    });
    const elapsed = performance.now() - started;
    drainStats.wakes += 1;
    drainStats.events += events.length;
    drainStats.ms += elapsed;
    drainStats.maxMs = Math.max(drainStats.maxMs, elapsed);
  }

  function connect(): void {
    closed = false;
    owner = new host.DesktopClient();
    batch(() => {
      setHandle(owner?.handle ?? "");
      setStatus("Connecting");
      setError(undefined);
      setAgents({});
      setServer(undefined);
    });
    owner.connect(
      // No geometry vote (0x0): attaching never reshapes panes another client
      // is showing; each pane sizes its own terminal explicitly.
      { socketPath: target.socketPath, cols: 0, rows: 0, sessionName: target.sessionName },
      activity,
    );
  }

  function close(): void {
    if (closed || !owner) return;
    closed = true;
    // The final batch below drains everything; a deferred drain would be
    // for the retired client, and must not hold back the next one's wake.
    clearTimeout(deferred);
    deferred = undefined;
    accept(owner.close());
  }

  function reconnect(): void {
    close();
    connect();
  }

  function homeSession(): DesktopSession | undefined {
    return topology()?.sessions.find((session) => session.name === target.sessionName);
  }

  function spawnSession(sessionId: number | undefined): DesktopSession | undefined {
    if (sessionId !== undefined) {
      return topology()?.sessions.find((session) => session.id === sessionId);
    }
    return homeSession();
  }

  function spawn({ sessionId, ...options }: SpawnRequest): number | undefined {
    const info = server();
    const session = spawnSession(sessionId);
    if (!info || !session || status() !== "Attached") return undefined;
    return client().spawnTerminalWithOptions({
      ...options,
      identity: { serverId: info.serverId, connectionEpoch: info.connectionEpoch },
      sessionId: session.id,
    });
  }

  return {
    status,
    error,
    topology,
    server,
    agents,
    revision,
    paintRevision: (terminalId) => Math.max(everyTerminal(), terminalRevision(terminalId)[0]()),
    handle,
    target,
    client,
    connect,
    reconnect,
    close,
    ready: (terminalId) => {
      revision();
      return (
        !closed &&
        !!owner &&
        knownTerminals.has(terminalId) &&
        owner.inputReadiness(terminalId).ready
      );
    },
    fenced: (terminalId) => {
      revision();
      return (
        !closed && !!owner && knownTerminals.has(terminalId) && owner.deliveryFenced(terminalId)
      );
    },
    homeSession,
    panes: () => topology()?.panes ?? [],
    refreshTopology: () => {
      if (closed || status() !== "Attached") return;
      client().refreshTopology();
    },
    spawn,
    onEvents: (next) => {
      listener = next;
    },
    onPathAnswers: (next) => {
      pathListener = next;
    },
  };
}
