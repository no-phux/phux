import { Show } from "solid-js";
import {
  Command,
  ComposerPill,
  MenuItem,
  Panel,
  SettingsPage,
  Style,
  TitlebarItem,
  type ComposerPillTone,
  type MountedSession,
  type PanelTab,
  type Plugin,
} from "@opencode/plugin/gui";

import type { Pane } from "../rpc.js";
import { errorMessage, phuxClient } from "./client.js";
import type { definition } from "./definition.js";
import { createFleet, fleetKey, isAgent, paneKey, paneName, stateKey } from "./fleet.js";
import { FleetView } from "./fleet-view.js";
import { PaneView } from "./pane-view.js";
import { SettingsView } from "./settings-view.js";
import { css } from "./style.js";
import { parseFormatterHtml, plainLines, screenText } from "./terminal.js";

const FLEET_TAB = "fleet";
const PANE_TAB = "pane:";
const TARGET = /^(?:[^/\s]+\/)?@[0-9]+$/;

const TONE: Readonly<Record<Pane["state"], ComposerPillTone | undefined>> = {
  blocked: "danger",
  working: "warning",
  done: "success",
  idle: undefined,
  unknown: undefined,
};

export const setup: Plugin.Setup<typeof definition> = (ctx) => {
  const client = phuxClient(ctx);
  const fleet = createFleet(ctx, client);

  ctx.add(Style, css);

  const current = (): MountedSession | undefined => {
    const session = ctx.sessions.current();
    if (session === undefined) ctx.toast.show({ title: ctx.t("no.session") });
    return session;
  };

  const spawn = async (session: MountedSession | undefined = current()): Promise<void> => {
    if (session === undefined) return;
    const owned = fleet.owned(session.id).length;
    if (owned >= fleet.state.maxTerminals) {
      ctx.toast.show({ variant: "error", title: ctx.t("spawn.failed"), description: ctx.t("spawn.limit", { count: owned }) });
      return;
    }
    try {
      const { target } = await client.spawn({ sessionID: session.id, cwd: session.directory }, { directory: session.directory });
      ctx.layout.open(paneKey(ctx, target), session, { tab: "select" });
      void fleet.refresh();
    } catch (cause) {
      ctx.toast.show({ variant: "error", title: ctx.t("spawn.failed"), description: errorMessage(cause) });
    }
  };

  const pick = (title: string, panes: readonly Pane[]): Promise<string | undefined> =>
    ctx.dialogs.select({
      title,
      placeholder: ctx.t("pick.pane.placeholder"),
      options: panes.map((pane) => ({
        title: paneName(pane),
        description: [pane.target, ctx.t(stateKey(pane)), pane.cwd ?? ""].filter(Boolean).join(" · "),
        value: pane.target,
        group: pane.session,
      })),
    });

  const openPicked = async (input?: string): Promise<void> => {
    const session = current();
    if (session === undefined) return;
    const typed = input?.trim();
    if (typed !== undefined && TARGET.test(typed)) {
      fleet.openPane(typed, session);
      return;
    }
    await fleet.refresh();
    const target = await pick(ctx.t("pick.pane"), fleet.state.panes);
    if (target !== undefined) fleet.openPane(target, session);
  };

  const promptAgent = async (): Promise<void> => {
    const session = ctx.sessions.current();
    await fleet.refresh();
    const target = await pick(ctx.t("pick.agent"), fleet.state.panes.filter(isAgent));
    if (target === undefined) return;
    const pane = fleet.pane(target);
    const text = await ctx.dialogs.prompt({
      title: ctx.t("prompt.title", { agent: pane === undefined ? target : paneName(pane) }),
      placeholder: ctx.t("prompt.placeholder"),
    });
    if (text === undefined) return;
    try {
      await client.prompt({ target, text }, session === undefined ? {} : { directory: session.directory });
      ctx.toast.show({ variant: "success", title: ctx.t("prompt.sent", { target }) });
      void fleet.refresh();
    } catch (cause) {
      ctx.toast.show({ variant: "error", title: ctx.t("pane.send.failed"), description: errorMessage(cause) });
    }
  };

  const quotePicked = async (): Promise<void> => {
    const session = current();
    const composer = ctx.screen.current()?.composer;
    if (session === undefined || composer === undefined || !composer.quotable()) return;
    await fleet.refresh();
    const target = await pick(ctx.t("command.quote"), fleet.state.panes);
    if (target === undefined) return;
    try {
      const screen = await client.screen({ target, history: 200 }, { directory: session.directory });
      const lines = screen.html !== undefined ? parseFormatterHtml(screen.html) : plainLines(screen.lines ?? []);
      const pane = fleet.pane(target);
      const commentID = crypto.randomUUID();
      composer.attach({
        type: "quote",
        commentID,
        text: screenText(lines, 80),
        source: {
          label: ctx.t("pane.source", { target }),
          icon: "console",
          subject: ctx.t("pane.subject", { target, title: pane === undefined ? target : paneName(pane) }),
        },
      });
      composer.focus(commentID);
    } catch (cause) {
      ctx.toast.show({ variant: "error", title: ctx.t("pane.error"), description: errorMessage(cause) });
    }
  };

  const goToAttention = (): void => {
    const [first] = fleet.blocked();
    if (first !== undefined) fleet.openPane(first.target);
  };

  const toggleFleet = (): void => {
    const session = current();
    if (session !== undefined) ctx.layout.toggle(fleetKey(ctx), session);
  };

  ctx.add(Command, (): Command => ({
    id: "fleet",
    title: ctx.t("command.fleet"),
    description: ctx.t("command.fleet.description"),
    group: ctx.t("command.group"),
    keywords: "phux panes agents terminals fleet tmux",
    section: "terminal",
    featured: true,
    enabled: ctx.sessions.current() !== undefined,
    slash: { name: "phux" },
    run: toggleFleet,
  }));
  ctx.add(Command, (): Command => ({
    id: "terminal",
    title: ctx.t("command.terminal"),
    description: ctx.t("command.terminal.description"),
    group: ctx.t("command.group"),
    keywords: "phux terminal shell spawn new pane",
    section: "terminal",
    enabled: ctx.sessions.current() !== undefined && fleet.state.available,
    run: () => void spawn(),
  }));
  ctx.add(Command, (): Command => ({
    id: "open",
    title: ctx.t("command.open"),
    group: ctx.t("command.group"),
    keywords: "phux pane terminal agent open attach",
    section: "terminal",
    enabled: ctx.sessions.current() !== undefined && fleet.state.available,
    run: (input) => void openPicked(input),
  }));
  ctx.add(Command, (): Command => ({
    id: "prompt",
    title: ctx.t("command.prompt"),
    group: ctx.t("command.group"),
    keywords: "phux agent prompt send message",
    section: "terminal",
    enabled: fleet.state.available && fleet.state.panes.some(isAgent),
    run: () => void promptAgent(),
  }));
  ctx.add(Command, (): Command => ({
    id: "quote",
    title: ctx.t("command.quote"),
    group: ctx.t("command.group"),
    keywords: "phux quote pane screen output context",
    section: "prompt",
    enabled: ctx.screen.current()?.composer.quotable() === true && fleet.state.available,
    run: () => void quotePicked(),
  }));
  ctx.add(Command, (): Command => ({
    id: "attention",
    title: ctx.t("command.attention"),
    group: ctx.t("command.group"),
    keywords: "phux blocked waiting approval attention",
    section: "terminal",
    suggested: true,
    enabled: fleet.blocked().length > 0 && ctx.sessions.list().length > 0,
    run: goToAttention,
  }));

  ctx.add(MenuItem, (): MenuItem => {
    const session = ctx.sessions.current();
    return {
      menu: "session.panel",
      id: "fleet",
      title: ctx.t("menu.fleet"),
      icon: "status",
      keybind: `${ctx.id}.fleet`,
      order: 60,
      ...(fleet.state.loaded && fleet.state.available ? { description: ctx.plural("fleet.summary.panes", fleet.state.panes.length) } : {}),
      opened: session !== undefined && ctx.layout.state(fleetKey(ctx), session) !== "closed",
      run: () => {
        if (session !== undefined) ctx.layout.open(fleetKey(ctx), session, { tab: "select" });
      },
    };
  });
  ctx.add(MenuItem, (): MenuItem => ({
    menu: "session.panel",
    id: "terminal",
    title: ctx.t("menu.terminal"),
    icon: "terminal",
    keybind: `${ctx.id}.terminal`,
    order: 61,
    run: () => void spawn(),
  }));

  const tabs = new Map<string, PanelTab>();
  const tab = (id: string): PanelTab => {
    const known = tabs.get(id);
    if (known !== undefined) return known;
    const created: PanelTab = id === FLEET_TAB
      ? {
        id,
        get title() {
          return ctx.t("panel.fleet");
        },
        closable: "hover",
        fallback: true,
        label: () => (
          <span class="phux-tab">
            <Show when={fleet.blocked().length > 0}>
              <span class="phux-dot" data-state="blocked" />
            </Show>
            <span>{ctx.t("panel.fleet")}</span>
          </span>
        ),
      }
      : paneTab(id.slice(PANE_TAB.length));
    tabs.set(id, created);
    return created;
  };
  const paneTab = (target: string): PanelTab => ({
    id: `${PANE_TAB}${target}`,
    get missing() {
      return fleet.state.loaded && fleet.state.available && fleet.pane(target) === undefined;
    },
    get title() {
      const pane = fleet.pane(target);
      return pane === undefined ? target : `${paneName(pane)} (${target})`;
    },
    closable: "hover",
    label: () => {
      const pane = () => fleet.pane(target);
      return (
        <span class="phux-tab" title={target}>
          <span class="phux-dot" data-state={pane()?.state ?? "unknown"} />
          <span>{pane() === undefined ? target : paneName(pane()!)}</span>
        </span>
      );
    },
  });

  ctx.add(Panel, {
    id: "phux",
    region: "side",
    mobile: {
      get title() {
        return ctx.t("name");
      },
      order: 60,
      kind: "menu",
      icon: "terminal",
    },
    list: (input) => input.open.filter((id) => id === FLEET_TAB || (id.startsWith(PANE_TAB) && TARGET.test(id.slice(PANE_TAB.length)))).map(tab),
    render: (props) => (
      <Show
        when={props.tab.id.startsWith(PANE_TAB)}
        fallback={<FleetView session={props.session} fleet={fleet} spawn={() => void spawn(props.session)} />}
      >
        <Show when={props.tab.id.slice(PANE_TAB.length)} keyed>
          {(target) => <PaneView target={target} session={props.session} screen={props.screen} fleet={fleet} client={client} />}
        </Show>
      </Show>
    ),
    closed: (input) => {
      if (!input.tab.id.startsWith(PANE_TAB)) return undefined;
      const target = input.tab.id.slice(PANE_TAB.length);
      return { title: input.tab.title, icon: "terminal", detail: target, data: target };
    },
    restore: (input) => {
      const target = input.data ?? (input.id.startsWith(PANE_TAB) ? input.id.slice(PANE_TAB.length) : undefined);
      if (target !== undefined && TARGET.test(target)) ctx.layout.open(paneKey(ctx, target), input.session);
      else ctx.layout.open(fleetKey(ctx), input.session);
    },
  });

  ctx.add(ComposerPill, {
    id: "terminals",
    order: 40,
    show: (input) => {
      const panes = fleet.owned(input.session.id).map((target) => fleet.pane(target) ?? unknownPane(target));
      if (panes.length === 0) return undefined;
      const tone = panes.some((pane) => pane.state === "blocked") ? "danger" : panes.some((pane) => pane.state === "working") ? "warning" : undefined;
      return {
        label: ctx.plural("pill.terminals", panes.length),
        icon: "terminal",
        title: ctx.t("pill.title"),
        ...(tone === undefined ? {} : { tone }),
        rows: panes.map((pane) => ({
          id: pane.target,
          label: `${paneName(pane)}`,
          icon: "terminal" as const,
          detail: `${pane.target} · ${ctx.t(stateKey(pane))}`,
          ...(TONE[pane.state] === undefined ? {} : { tone: TONE[pane.state]! }),
          run: () => fleet.openPane(pane.target, input.session),
        })),
      };
    },
  });

  ctx.add(TitlebarItem, (): TitlebarItem | undefined => {
    const blocked = fleet.blocked();
    if (blocked.length === 0) return undefined;
    return {
      id: "attention",
      label: ctx.plural("titlebar.attention", blocked.length),
      icon: "warning",
      run: goToAttention,
    };
  });

  ctx.add(SettingsPage, {
    id: "phux",
    icon: "terminal",
    get title() {
      return ctx.t("settings.title");
    },
    get entries() {
      return (["notifyBlocked", "notifyFinished", "openSpawned", "fitWidth"] as const).map((key) => ({
        id: key,
        title: ctx.t(`settings.${key}`),
        description: ctx.t(`settings.${key}.description`),
        keywords: "phux terminal agent notification",
      }));
    },
    render: () => <SettingsView fleet={fleet} />,
  });

  return () => fleet.stop();
};

function unknownPane(target: string): Pane {
  return {
    target,
    session: "",
    title: null,
    cwd: null,
    agent: { id: "unknown", label: "", kind: "unknown" },
    state: "unknown",
    attention: "none",
    provider: null,
    nativeId: null,
    seq: 0,
    since: 0,
  };
}
