import { For, Show, type JSX, type Accessor } from "solid-js";
import { IconButton, Label, StatusDot, row, usePalette } from "../ui/controls";
import { radius } from "../ui/theme";

export interface TabView {
  id: string;
  title: string;
  panes: number;
  agentState?: string;
  zoomed: boolean;
}

export const TITLEBAR_HEIGHT = 40;
/** Room for the macOS traffic lights inside a transparent titlebar. */
export const TRAFFIC_LIGHTS = 78;

/**
 * The titlebar row: a drag region fills every empty pixel, tabs and buttons
 * sit on top of it. Tabs show an agent dot, a pane count, and close on hover.
 */
export function TabBar(props: {
  tabs: TabView[];
  activeId: string;
  sidebarWidth: number;
  sidebarVisible: boolean;
  select: (id: string) => void;
  close: (id: string) => void;
  newTab: () => void;
  toggleSidebar: () => void;
  splitRight: () => void;
  splitDown: () => void;
  openPalette: () => void;
  beginDrag: (id: string, x: number) => void;
  rename: (id: string) => void;
  registerTab: (id: string, element: number | undefined) => void;
}): JSX.Element {
  const colors = usePalette();
  return (
    <div style={{ position: "relative", height: TITLEBAR_HEIGHT, flexShrink: 0 }}>
      <phux-drag-region style={{ position: "absolute", top: 0, left: 0, right: 0, bottom: 0 }} />
      <div
        style={row({
          position: "absolute",
          top: 0,
          left: 0,
          right: 0,
          bottom: 0,
          pointerEvents: "none",
        })}
      >
        <div
          style={row({
            width: props.sidebarVisible ? props.sidebarWidth : TRAFFIC_LIGHTS + 36,
            height: TITLEBAR_HEIGHT,
            flexShrink: 0,
            paddingLeft: TRAFFIC_LIGHTS,
            paddingRight: 8,
            gap: 2,
            backgroundColor: props.sidebarVisible ? colors().surface : "transparent",
          })}
        >
          <IconButton
            icon="sidebar"
            label="Toggle sidebar"
            run={() => props.toggleSidebar()}
            active={props.sidebarVisible}
          />
          <div style={{ flexGrow: 1 }} />
        </div>
        <div
          style={row({ flexGrow: 1, flexShrink: 1, gap: 4, paddingLeft: 8, overflow: "hidden" })}
        >
          <For each={props.tabs}>
            {(tab): JSX.Element => (
              <Tab
                tab={tab}
                active={tab.id === props.activeId}
                select={() => props.select(tab.id)}
                close={() => props.close(tab.id)}
                beginDrag={(x) => props.beginDrag(tab.id, x)}
                rename={() => props.rename(tab.id)}
                register={(element) => props.registerTab(tab.id, element)}
              />
            )}
          </For>
          <IconButton icon="plus" label="New tab" run={() => props.newTab()} size={24} />
        </div>
        <div style={row({ gap: 2, paddingRight: 10, flexShrink: 0 })}>
          <IconButton icon="splitRight" label="Split right" run={() => props.splitRight()} />
          <IconButton icon="splitDown" label="Split down" run={() => props.splitDown()} />
          <IconButton icon="command" label="Command palette" run={() => props.openPalette()} />
        </div>
      </div>
    </div>
  );
}

function Tab(props: {
  tab: TabView;
  active: boolean;
  select: () => void;
  close: () => void;
  beginDrag: (x: number) => void;
  rename: () => void;
  register: (element: number | undefined) => void;
}): JSX.Element {
  const colors = usePalette();
  return (
    <div
      ref={(element: { id: number }): JSX.Element => props.register(element.id)}
      role="tab"
      aria-selected={props.active}
      aria-label={props.tab.title}
      onMouseDown={(event) => {
        props.select();
        if ((event.clickCount ?? 1) >= 2) props.rename();
        else props.beginDrag(event.x ?? 0);
      }}
      style={row({
        gap: 7,
        height: 28,
        minWidth: 88,
        maxWidth: 220,
        flexShrink: 1,
        paddingLeft: 10,
        paddingRight: 4,
        borderRadius: radius.control,
        cursor: "pointer",
        pointerEvents: "auto",
        backgroundColor: props.active ? colors().active : "transparent",
        borderWidth: 1,
        borderColor: props.active ? colors().border : "transparent",
        hover: { backgroundColor: props.active ? colors().active : colors().hover },
      })}
    >
      <Show when={props.tab.agentState}>
        {(state: Accessor<string>): JSX.Element => <StatusDot state={state()} size={7} />}
      </Show>
      <Label
        grow
        weight={props.active ? 600 : 500}
        color={props.active ? colors().foreground : colors().muted}
      >
        {props.tab.title}
      </Label>
      <Show when={props.tab.panes > 1}>
        <text style={{ color: colors().faint, fontSize: 10.5, fontWeight: 600 }}>
          {props.tab.zoomed ? `⤢ ${props.tab.panes}` : String(props.tab.panes)}
        </text>
      </Show>
      <IconButton
        icon="close"
        label={`Close ${props.tab.title}`}
        run={() => props.close()}
        size={18}
        color={colors().faint}
      />
    </div>
  );
}
