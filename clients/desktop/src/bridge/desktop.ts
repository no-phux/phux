/**
 * The typed native boundary. One `DesktopClient` per connection, one drain
 * per wake: this module owns `takeEvents`, turns the batch into Solid
 * signals, and forwards the raw batch to one subscriber. UI code never
 * touches the socket, parses VT, or retries delivery.
 */
import { batch, createSignal, type Accessor } from "solid-js";
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
  spawn(options: Omit<DesktopSpawnOptions, "identity" | "sessionId">): number | undefined;
  onEvents(listener: (events: DesktopEvent[]) => void): void;
  /** Host path answers (`PATH_QUERY`), drained on the same wake as events. */
  onPathAnswers(listener: (answers: DesktopPathAnswer[]) => void): void;
}

/**
 * Wake drains across every bridge in this process, for `PHUX_DESKTOP_PERF`:
 * how many wakes, how many events they carried, and the milliseconds spent
 * draining and applying them (including the Solid updates they trigger).
 */
export const drainStats = { wakes: 0, events: 0, ms: 0, maxMs: 0 };

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

export function createBridge(host: DesktopHost, target: ConnectTarget): Bridge {
  const [status, setStatus] = createSignal("Connecting");
  const [error, setError] = createSignal<string | undefined>();
  const [topology, setTopology] = createSignal<DesktopTopology | undefined>();
  const [server, setServer] = createSignal<DesktopServerInfo | undefined>();
  const [agents, setAgents] = createSignal<Record<string, AgentInfo>>({});
  const [revision, setRevision] = createSignal(0);
  const [handle, setHandle] = createSignal("");
  let owner: DesktopClient | undefined;
  let closed = false;
  let listener: (events: DesktopEvent[]) => void = () => {};
  let pathListener: (answers: DesktopPathAnswer[]) => void = () => {};

  function client(): DesktopClient {
    if (!owner) throw new Error("Desktop client is not connected");
    return owner;
  }

  function accept(events: DesktopEvent[]): void {
    for (const event of events) {
      if (event.kind === "ServerError") setError(event.message);
      if (event.kind === "AgentBadge") noteAgent(event);
    }
    listener(events);
  }

  function noteAgent(event: Extract<DesktopEvent, { kind: "AgentBadge" }>): void {
    setAgents((current) => {
      const next = { ...current };
      if (!event.name) {
        delete next[event.terminalId];
        return next;
      }
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

  function snapshot(): void {
    const native = client();
    const failure = native.lastError();
    const next = native.topology() ?? undefined;
    batch(() => {
      setStatus(native.status());
      if (failure) setError(failure);
      setTopology(next);
      const info = native.serverInfo();
      if (info) setServer(info);
    });
  }

  function activity(from: string): void {
    if (closed || !owner || from !== owner.handle) return;
    const started = performance.now();
    // One drain per wake, even when empty: the drain rearms notification.
    const events = owner.takeEvents();
    // Drain every wake: the runtime's answer queue is bounded, and a full
    // queue refuses the next query.
    const answers = owner.takePathAnswers();
    batch(() => {
      accept(events);
      if (answers.length > 0) pathListener(answers);
      // A new topology object re-renders every row derived from it, so read
      // one only when an event can have changed it: output, receipts and
      // agent badges cannot, and under a flood they are nearly every wake.
      if (events.some(structural)) snapshot();
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
    accept(owner.close());
  }

  function reconnect(): void {
    close();
    connect();
  }

  function homeSession(): DesktopSession | undefined {
    return topology()?.sessions.find((session) => session.name === target.sessionName);
  }

  function spawn(options: Omit<DesktopSpawnOptions, "identity" | "sessionId">): number | undefined {
    const info = server();
    const home = homeSession();
    if (!info || !home || status() !== "Attached") return undefined;
    return client().spawnTerminalWithOptions({
      ...options,
      identity: { serverId: info.serverId, connectionEpoch: info.connectionEpoch },
      sessionId: home.id,
    });
  }

  return {
    status,
    error,
    topology,
    server,
    agents,
    revision,
    handle,
    target,
    client,
    connect,
    reconnect,
    close,
    ready: (terminalId) => {
      revision();
      return !closed && !!owner && owner.inputReadiness(terminalId).ready;
    },
    fenced: (terminalId) => {
      revision();
      return !closed && !!owner && owner.inputReadiness(terminalId).deliveryFenced;
    },
    homeSession,
    panes: () => topology()?.panes ?? [],
    spawn,
    onEvents: (next) => {
      listener = next;
    },
    onPathAnswers: (next) => {
      pathListener = next;
    },
  };
}
