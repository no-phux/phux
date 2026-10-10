import { createMemo, createSignal, For, Show, type JSX, type Accessor } from "solid-js";
import type { DesktopPane, DesktopSession } from "../../native/generated/index";
import type { AgentInfo } from "../bridge/desktop";
import { IconButton, Icon, Label, Pill, StatusDot, column, row, usePalette } from "../ui/controls";
import { agentColors, radius, uiFont } from "../ui/theme";
import { groupByProject, terminalHost } from "../workspace/place";

export interface SidebarProps {
  width: number;
  sessions: DesktopSession[];
  panes: DesktopPane[];
  agents: Record<string, AgentInfo>;
  homeSession: string;
  focusedTerminal: string | undefined;
  visibleTerminals: Set<string>;
  viewCount: (terminalId: string) => number;
  now: number;
  open: (pane: DesktopPane) => void;
  newTerminal: () => void;
  /** Spawn in this session. The header + calls this instead of the home session. */
  newTerminalIn?: (sessionId: number) => void;
  /** Rename this session from the header menu. */
  renameSession?: (name: string) => void;
  /** Close this session from the header menu. */
  closeSession?: (name: string) => void;
  /** Project tags keyed by session id. Empty keeps the flat session list. */
  projects?: ReadonlyMap<number, string>;
  openSettings: () => void;
  openPalette: () => void;
}

const PRIORITY: Record<string, number> = { blocked: 0, done: 1, working: 2, idle: 3, unknown: 4 };

/** Two sections: agents by urgency, then every server session and its panes. */
export function Sidebar(props: SidebarProps): JSX.Element {
  const colors = usePalette();
  const [collapsed, setCollapsed] = createSignal<Record<string, boolean>>({});
  const [menu, setMenu] = createSignal<number | undefined>();
  const agentPanes = createMemo(() => {
    const agents = props.agents;
    return props.panes
      .filter((pane) => agents[pane.terminalId])
      .sort((a, b) => {
        const left = agents[a.terminalId];
        const right = agents[b.terminalId];
        const rank =
          (PRIORITY[left?.state ?? "unknown"] ?? 4) - (PRIORITY[right?.state ?? "unknown"] ?? 4);
        return rank !== 0 ? rank : (right?.changedAt ?? 0) - (left?.changedAt ?? 0);
      });
  });
  const sessions = createMemo(() => {
    const home = props.homeSession;
    return [...props.sessions].sort((a, b) =>
      a.name === home ? -1 : b.name === home ? 1 : a.name.localeCompare(b.name),
    );
  });
  const sessionGroups = createMemo(() => {
    const tagged = sessions().map((session) => ({
      name: session.name,
      project: props.projects?.get(session.id),
      host: sessionHost(session.id, props.panes),
      session,
    }));
    return groupByProject(tagged).map((group) => ({
      project: group.project,
      host: group.host,
      sessions: group.sessions.map((line) => line.session),
    }));
  });
  const hasGroups = createMemo(
    () =>
      (props.projects !== undefined && props.projects.size > 0) ||
      props.panes.some((pane) => terminalHost(pane.terminalId) !== undefined),
  );

  return (
    <div
      style={column({
        width: props.width,
        flexShrink: 0,
        backgroundColor: colors().surface,
        overflow: "hidden",
      })}
    >
      <div
        style={column({
          flexGrow: 1,
          overflowY: "scroll",
          paddingLeft: 8,
          paddingRight: 8,
          paddingBottom: 8,
          gap: 2,
        })}
      >
        <Show when={agentPanes().length > 0}>
          <SectionHeader title="Agents" count={agentPanes().length} />
          <For each={agentPanes()}>
            {(pane): JSX.Element => (
              <AgentRow
                pane={pane}
                agent={props.agents[pane.terminalId]}
                now={props.now}
                focused={props.focusedTerminal === pane.terminalId}
                open={() => props.open(pane)}
              />
            )}
          </For>
          <div style={{ height: 8 }} />
        </Show>
        <SectionHeader title="Sessions" count={props.sessions.length} />
        <For each={sessionGroups()}>
          {(group): JSX.Element => (
            <div style={column({ gap: 1 })}>
              <Show when={hasGroups()}>
                <SectionHeader title={groupTitle(group)} count={group.sessions.length} />
              </Show>
              <For each={group.sessions}>
                {(session): JSX.Element => {
                  const panes = (): DesktopPane[] =>
                    props.panes.filter((pane) => pane.sessionId === session.id);
                  const closed = (): boolean => collapsed()[session.name] === true;
                  return (
                    <div style={column({ gap: 1 })}>
                      <div
                        onClick={() =>
                          setCollapsed((current) => ({ ...current, [session.name]: !closed() }))
                        }
                        style={row({
                          gap: 6,
                          height: 26,
                          paddingLeft: 6,
                          paddingRight: 8,
                          borderRadius: radius.small,
                          cursor: "pointer",
                          hover: { backgroundColor: colors().hover },
                        })}
                      >
                        <Icon
                          name={closed() ? "chevronRight" : "chevronDown"}
                          size={11}
                          color={colors().faint}
                        />
                        <Label weight={600} color={colors().subtext} grow>
                          {session.name}
                        </Label>
                        <Show when={session.name === props.homeSession}>
                          <Pill color={colors().accent} subtle>
                            home
                          </Pill>
                        </Show>
                        <Label size={uiFont.small} color={colors().faint}>
                          {String(panes().length)}
                        </Label>
                        <Show when={props.newTerminalIn}>
                          <IconButton
                            icon="plus"
                            label={`New terminal in ${session.name}`}
                            run={() => props.newTerminalIn?.(session.id)}
                          />
                        </Show>
                        <Show when={props.renameSession || props.closeSession}>
                          <IconButton
                            icon="command"
                            label={`Session menu for ${session.name}`}
                            run={() =>
                              setMenu((current) =>
                                current === session.id ? undefined : session.id,
                              )
                            }
                          />
                        </Show>
                      </div>
                      <Show when={menu() === session.id}>
                        <div style={row({ gap: 6, paddingLeft: 22, paddingBottom: 4 })}>
                          <Show when={props.renameSession}>
                            <IconButton
                              icon="terminal"
                              label={`Rename ${session.name}`}
                              run={() => {
                                setMenu(undefined);
                                props.renameSession?.(session.name);
                              }}
                            />
                          </Show>
                          <Show when={props.closeSession}>
                            <IconButton
                              icon="close"
                              label={`Close ${session.name}`}
                              run={() => {
                                setMenu(undefined);
                                props.closeSession?.(session.name);
                              }}
                            />
                          </Show>
                        </div>
                      </Show>
                      <Show when={!closed()}>
                        <For each={panes()}>
                          {(pane): JSX.Element => (
                            <PaneRow
                              pane={pane}
                              agent={props.agents[pane.terminalId]}
                              focused={props.focusedTerminal === pane.terminalId}
                              visible={props.visibleTerminals.has(pane.terminalId)}
                              views={props.viewCount(pane.terminalId)}
                              open={() => props.open(pane)}
                            />
                          )}
                        </For>
                      </Show>
                    </div>
                  );
                }}
              </For>
            </div>
          )}
        </For>
        <Show when={props.sessions.length === 0}>
          <div style={{ padding: 10 }}>
            <Label color={colors().muted}>No sessions yet.</Label>
          </div>
        </Show>
      </div>
      <div style={{ height: 1, backgroundColor: colors().border }} />
      <div style={row({ height: 40, paddingLeft: 8, paddingRight: 8, gap: 2 })}>
        <IconButton icon="plus" label="New terminal" run={() => props.newTerminal()} />
        <IconButton icon="command" label="Command palette" run={() => props.openPalette()} />
        <div style={{ flexGrow: 1 }} />
        <IconButton icon="settings" label="Settings" run={() => props.openSettings()} />
      </div>
    </div>
  );
}

function sessionHost(sessionId: number, panes: readonly DesktopPane[]): string | undefined {
  const hosts = [
    ...new Set(
      panes
        .filter((pane) => pane.sessionId === sessionId)
        .map((pane) => terminalHost(pane.terminalId))
        .filter((host): host is string => host !== undefined),
    ),
  ];
  return hosts.length === 1 ? hosts[0] : hosts.length > 1 ? "mixed remote" : undefined;
}

function groupTitle(group: { project?: string | undefined; host?: string | undefined }): string {
  if (group.project && group.host) return `${group.project} · ${group.host}`;
  return group.project ?? group.host ?? "Local sessions";
}

function SectionHeader(props: { title: string; count: number }): JSX.Element {
  const colors = usePalette();
  return (
    <div style={row({ height: 28, paddingLeft: 8, paddingRight: 8, paddingTop: 6 })}>
      <text
        style={{
          color: colors().faint,
          fontSize: uiFont.small - 0.5,
          fontWeight: 700,
          flexGrow: 1,
        }}
      >
        {props.title.toUpperCase()}
      </text>
      <Label size={uiFont.small} color={colors().faint}>
        {String(props.count)}
      </Label>
    </div>
  );
}

function PaneRow(props: {
  pane: DesktopPane;
  agent: AgentInfo | undefined;
  focused: boolean;
  visible: boolean;
  views: number;
  open: () => void;
}): JSX.Element {
  const colors = usePalette();
  return (
    <div
      onClick={() => props.open()}
      style={row({
        gap: 9,
        minHeight: 42,
        paddingLeft: 10,
        paddingRight: 8,
        paddingTop: 5,
        paddingBottom: 5,
        borderRadius: radius.control,
        cursor: "pointer",
        backgroundColor: props.focused ? colors().active : "transparent",
        hover: { backgroundColor: props.focused ? colors().active : colors().hover },
      })}
    >
      <div style={row({ width: 14, justifyContent: "center", flexShrink: 0 })}>
        <Show when={!props.agent}>
          <Icon
            name="terminal"
            size={13}
            color={props.visible ? colors().accent : colors().faint}
          />
        </Show>
        <Show when={props.agent}>
          {(agent: Accessor<AgentInfo>): JSX.Element => <StatusDot state={agent().state} />}
        </Show>
      </div>
      <div style={column({ flexGrow: 1, flexShrink: 1, gap: 2, overflow: "hidden" })}>
        <Label
          weight={props.focused ? 600 : 500}
          color={props.focused ? colors().foreground : colors().subtext}
        >
          {paneTitle(props.pane)}
        </Label>
        <Label size={uiFont.small} color={colors().muted}>
          {shortPath(props.pane.cwd) || props.pane.terminalId}
        </Label>
      </div>
      <Show when={props.views > 1}>
        <Pill color={colors().muted} subtle>
          {`${props.views} views`}
        </Pill>
      </Show>
    </div>
  );
}

function AgentRow(props: {
  pane: DesktopPane;
  agent: AgentInfo | undefined;
  now: number;
  focused: boolean;
  open: () => void;
}): JSX.Element {
  const colors = usePalette();
  const state = (): string => props.agent?.state ?? "unknown";
  const tint = (): string => agentColors[state()] ?? colors().muted;
  const urgent = (): boolean => props.agent?.attention === "high";
  return (
    <div
      onClick={() => props.open()}
      style={row({
        gap: 9,
        minHeight: 42,
        paddingLeft: 10,
        paddingRight: 8,
        paddingTop: 5,
        paddingBottom: 5,
        borderRadius: radius.control,
        cursor: "pointer",
        borderWidth: 1,
        borderColor: urgent() ? `${tint()}66` : "transparent",
        backgroundColor: props.focused ? colors().active : urgent() ? `${tint()}14` : "transparent",
        hover: { backgroundColor: colors().hover },
      })}
    >
      <div style={row({ width: 14, justifyContent: "center", flexShrink: 0 })}>
        <StatusDot state={state()} />
      </div>
      <div style={column({ flexGrow: 1, flexShrink: 1, gap: 2, overflow: "hidden" })}>
        <Label weight={600}>{props.agent?.name ?? "agent"}</Label>
        <Label size={uiFont.small} color={colors().muted}>
          {paneTitle(props.pane)}
        </Label>
      </div>
      <div style={column({ alignItems: "flex-end", gap: 2, flexShrink: 0 })}>
        <Label size={uiFont.small} weight={600} color={tint()}>
          {state()}
        </Label>
        <Label size={uiFont.small - 1} color={colors().faint}>
          {ago(props.agent?.changedAt ?? props.now, props.now)}
        </Label>
      </div>
    </div>
  );
}

export function paneTitle(pane: DesktopPane | undefined): string {
  if (!pane) return "terminal";
  const title = pane.title?.trim();
  if (title) return title;
  const base = pane.cwd ? pane.cwd.split("/").filter(Boolean).at(-1) : undefined;
  return base ?? pane.windowName ?? "shell";
}

export function shortPath(path: string | undefined): string {
  if (!path) return "";
  const home = /^\/Users\/[^/]+|^\/home\/[^/]+/.exec(path)?.[0];
  return home ? `~${path.slice(home.length)}` : path;
}

export function ago(then: number, now: number): string {
  const seconds = Math.max(0, Math.round((now - then) / 1000));
  if (seconds < 5) return "now";
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  return hours < 24 ? `${hours}h` : `${Math.round(hours / 24)}d`;
}
