import { createMemo, createSignal, For, Show, type JSX } from "solid-js";
import { usePanel, usePlugin, type MountedSession } from "@opencode/plugin/gui";

import type { Pane } from "../rpc.js";
import type { definition } from "./definition.js";
import { isAgent, paneName, showsSession, stateKey, type Fleet } from "./fleet.js";
import { shortPath } from "./terminal.js";

export interface FleetViewProps {
  readonly session: MountedSession;
  readonly fleet: Fleet;
  readonly spawn: () => void;
}

interface Group {
  readonly id: string;
  readonly label: string;
  readonly panes: readonly Pane[];
}

/** Every pane on the phux server, with this session's and the ones that need you first. */
export function FleetView(props: FleetViewProps): JSX.Element {
  const ctx = usePlugin<typeof definition>();
  const panel = usePanel();
  const ui = ctx.ui;
  const [query, setQuery] = createSignal("");

  const mine = (pane: Pane): boolean =>
    props.fleet.owned(props.session.id).includes(pane.target) || showsSession(pane, props.session.id);
  const inProject = (pane: Pane): boolean =>
    pane.cwd !== null && (pane.cwd === props.session.directory || pane.cwd.startsWith(`${props.session.directory}/`));

  const filtered = createMemo(() => {
    const words = query().toLowerCase().split(/\s+/).filter((word) => word.length > 0);
    if (words.length === 0) return props.fleet.state.panes;
    return props.fleet.state.panes.filter((pane) => {
      const haystack = [pane.target, paneName(pane), pane.session, pane.agent.label, pane.cwd ?? "", ctx.t(stateKey(pane))]
        .join(" ")
        .toLowerCase();
      return words.every((word) => haystack.includes(word));
    });
  });

  const groups = createMemo((): Group[] => {
    const own: Pane[] = [];
    const attention: Pane[] = [];
    const bySession = new Map<string, Pane[]>();
    for (const pane of filtered()) {
      if (mine(pane)) own.push(pane);
      else if (pane.state === "blocked") attention.push(pane);
      else bySession.set(pane.session, [...(bySession.get(pane.session) ?? []), pane]);
    }
    const sessions = [...bySession.entries()]
      .sort(([left, a], [right, b]) => Number(b.some(inProject)) - Number(a.some(inProject)) || left.localeCompare(right))
      .map(([session, panes]) => ({ id: `session:${session}`, label: ctx.t("fleet.group.other", { session }), panes }));
    return [
      ...(own.length === 0 ? [] : [{ id: "own", label: ctx.t("fleet.group.session"), panes: own }]),
      ...(attention.length === 0 ? [] : [{ id: "attention", label: ctx.t("fleet.group.attention"), panes: attention }]),
      ...sessions,
    ];
  });

  const summary = () => {
    const parts = [ctx.plural("fleet.summary.panes", props.fleet.state.panes.length)];
    const working = props.fleet.working().length;
    const blocked = props.fleet.blocked().length;
    if (working > 0) parts.push(ctx.t("fleet.summary.working", { count: working }));
    if (blocked > 0) parts.push(ctx.t("fleet.summary.blocked", { count: blocked }));
    return parts.join(" · ");
  };

  const metaLine = (pane: Pane): string => {
    const name = paneName(pane);
    const path = shortPath(pane.cwd);
    const agent = isAgent(pane) ? (pane.agent.label === name ? pane.agent.kind.replace("_", "") : pane.agent.label) : undefined;
    const where = path === name || path.endsWith(`/${name}`) ? undefined : path;
    return [pane.target, agent, where].filter(Boolean).join(" · ");
  };

  const row = (pane: Pane) => (
    <button type="button" class="phux-row" onClick={() => props.fleet.openPane(pane.target, props.session)} title={pane.cwd ?? pane.target}>
      <span class="phux-dot" data-state={pane.state} />
      <span class="phux-row-text">
        <span class="phux-row-title">
          {paneName(pane)}
          <Show when={props.fleet.owned(props.session.id).includes(pane.target)}>
            <span class="phux-badge">{ctx.t("fleet.owned")}</span>
          </Show>
          <Show when={showsSession(pane, props.session.id)}>
            <span class="phux-badge">{ctx.t("fleet.shows")}</span>
          </Show>
        </span>
        <span class="phux-row-meta">
          {metaLine(pane)}
        </span>
      </span>
      <span class="phux-row-state" data-state={pane.state}>{ctx.t(stateKey(pane))}</span>
    </button>
  );

  return (
    <div class="phux-root" classList={{ "phux-dark": ctx.appearance.mode() === "dark", "phux-light": ctx.appearance.mode() === "light" }}>
      <div class="phux-header">
        <div class="phux-header-text">
          <span class="phux-header-title">{ctx.t("panel.fleet.title")}</span>
          <span class="phux-header-meta">{props.fleet.state.available ? summary() : ctx.t("fleet.unavailable")}</span>
        </div>
        <div class="phux-header-actions">
          <ui.Tooltip value={ctx.t("command.terminal")}>
            <ui.IconButton icon="plus" label={ctx.t("command.terminal")} variant="ghost" size="small" onClick={props.spawn} />
          </ui.Tooltip>
          <ui.Tooltip value={ctx.t("fleet.refresh")}>
            <ui.IconButton icon="refresh" label={ctx.t("fleet.refresh")} variant="ghost" size="small" onClick={() => void props.fleet.refresh()} />
          </ui.Tooltip>
        </div>
        <Show when={panel.reserve()}>
          <span class="phux-header-reserve" />
        </Show>
      </div>

      <Show
        when={props.fleet.state.loaded}
        fallback={
          <div class="phux-empty">
            <ui.Loader size="large" />
            <span>{ctx.t("fleet.loading")}</span>
          </div>
        }
      >
        <Show
          when={props.fleet.state.available}
          fallback={
            <div class="phux-empty">
              <span class="phux-empty-title">{ctx.t("fleet.unavailable")}</span>
              <span>{ctx.t("fleet.unavailable.hint")}</span>
              <Show when={props.fleet.state.error}>{(error) => <span class="phux-error">{error()}</span>}</Show>
              <ui.Button size="small" variant="neutral" icon="refresh" onClick={() => void props.fleet.refresh()}>
                {ctx.t("pane.retry")}
              </ui.Button>
            </div>
          }
        >
          <div class="phux-search">
            <ui.TextInput type="search" value={query()} onInput={setQuery} label={ctx.t("fleet.search")} placeholder={ctx.t("fleet.search")} />
          </div>
          <div class="phux-list">
            <Show
              when={groups().length > 0}
              fallback={<div class="phux-empty">{props.fleet.state.panes.length === 0 ? ctx.t("fleet.empty") : ctx.t("fleet.nomatch")}</div>}
            >
              <For each={groups()}>
                {(group) => (
                  <section>
                    <div class="phux-group">
                      <span>{group.label}</span>
                      <span class="phux-group-count">{group.panes.length}</span>
                    </div>
                    <For each={group.panes}>{row}</For>
                  </section>
                )}
              </For>
            </Show>
          </div>
        </Show>
      </Show>
    </div>
  );
}
