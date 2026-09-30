import { Show, type JSX, type Accessor } from "solid-js";
import type { DesktopPane } from "../../native/generated/index";
import type { AgentInfo } from "../bridge/desktop";
import { paneTitle, shortPath } from "../shell/sidebar";
import type { HostAction, TerminalTheme } from "../terminal-element";
import { IconButton, Label, Pill, StatusDot, row, usePalette } from "../ui/controls";
import { agentColors, uiFont } from "../ui/theme";
import type { Placement } from "../workspace/layout";

export interface PaneProps {
  placement: Placement;
  clientHandle: string;
  pane: DesktopPane | undefined;
  agent: AgentInfo | undefined;
  /** The tab's focused placement. */
  selected: boolean;
  /** Selected and no overlay holds the keyboard. */
  inputFocused: boolean;
  /** This view proposes the terminal's PTY size from its bounds. */
  sizeOwner: boolean;
  showHeader: boolean;
  zoomed: boolean;
  fenced: boolean;
  views: number;
  revision: number;
  font: { family: string; size: number; lineHeight: number; cellWidth: number; cellHeight: number };
  /** Non-Command chords the shell binds; the terminal lets them through. */
  appChords: string[];
  /** A clipboard or open request for this terminal, run once per new id. */
  hostAction?: HostAction | undefined;
  padding: { x: number; y: number };
  /** 0 for none, else how strongly an unfocused pane is dimmed. */
  dim: number;
  theme: TerminalTheme;
  optionAsAlt: boolean;
  /** Overlays drawn over the terminal, such as the find bar. */
  children?: JSX.Element;
  focus: () => void;
  close: () => void;
  splitRight: () => void;
  splitDown: () => void;
  zoom: () => void;
  drop: (paths: string[]) => void;
}

/**
 * One placement: an optional header strip, then the native terminal. The
 * terminal paints runtime frames directly; this component only frames it.
 */
export function Pane(props: PaneProps): JSX.Element {
  const colors = usePalette();
  const tint = (): string =>
    props.agent ? (agentColors[props.agent.state] ?? colors().accent) : colors().accent;
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        flexGrow: 1,
        minWidth: 0,
        minHeight: 0,
        backgroundColor: props.theme.background,
        borderWidth: props.showHeader ? 1 : 0,
        borderColor: props.selected && props.showHeader ? `${colors().accent}66` : colors().border,
        borderRadius: props.showHeader ? 6 : 0,
        overflow: "hidden",
      }}
    >
      <Show when={props.showHeader}>
        <div
          onClick={(event) => {
            props.focus();
            if ((event.clickCount ?? 1) >= 2) props.zoom();
          }}
          style={row({
            height: 28,
            flexShrink: 0,
            gap: 8,
            paddingLeft: 10,
            paddingRight: 4,
            backgroundColor: props.selected ? colors().raised : colors().surface,
            borderBottomWidth: 1,
            borderColor: colors().border,
          })}
        >
          <Show when={!props.agent}>
            <div
              style={{
                width: 6,
                height: 6,
                borderRadius: 3,
                backgroundColor: props.selected ? colors().accent : colors().faint,
              }}
            />
          </Show>
          <Show when={props.agent}>
            {(agent: Accessor<AgentInfo>): JSX.Element => (
              <StatusDot state={agent().state} size={7} />
            )}
          </Show>
          <Label
            keep
            weight={props.selected ? 600 : 500}
            color={props.selected ? colors().foreground : colors().muted}
          >
            {paneTitle(props.pane)}
          </Label>
          <Label size={uiFont.small} color={colors().faint} grow>
            {shortPath(props.pane?.cwd)}
          </Label>
          <Show when={props.agent}>
            {(agent: Accessor<AgentInfo>): JSX.Element => (
              <Pill color={tint()} subtle>
                {`${agent().name} · ${agent().state}`}
              </Pill>
            )}
          </Show>
          <Show when={props.views > 1}>
            <Pill color={colors().muted} subtle>
              {`${props.views} views`}
            </Pill>
          </Show>
          <Show when={props.fenced}>
            <Pill color={colors().warning}>delivery unknown</Pill>
          </Show>
          <IconButton
            icon="splitRight"
            label="Split right"
            run={() => props.splitRight()}
            size={22}
          />
          <IconButton icon="splitDown" label="Split down" run={() => props.splitDown()} size={22} />
          <IconButton
            icon={props.zoomed ? "minimize" : "maximize"}
            label={props.zoomed ? "Restore pane" : "Zoom pane"}
            run={() => props.zoom()}
            size={22}
          />
          <IconButton icon="close" label="Close pane" run={() => props.close()} size={22} />
        </div>
      </Show>
      <div
        style={{
          display: "flex",
          position: "relative",
          flexGrow: 1,
          minWidth: 0,
          minHeight: 0,
          paddingTop: props.padding.y,
          paddingBottom: props.padding.y,
          paddingLeft: props.padding.x,
          paddingRight: props.padding.x,
          backgroundColor: props.theme.background,
        }}
      >
        <phux-terminal
          clientHandle={props.clientHandle}
          terminalId={props.placement.terminalId}
          viewId={props.placement.viewId}
          paintRevision={props.revision}
          focused={props.inputFocused}
          sizeOwner={props.sizeOwner}
          optionAsAlt={props.optionAsAlt}
          appChords={props.appChords}
          hostAction={props.hostAction}
          font={props.font}
          theme={props.theme}
          onClick={() => props.focus()}
          onFileDrop={(event) => props.drop(event.paths ?? [])}
          style={{ flexGrow: 1, minWidth: 0, minHeight: 0 }}
        />
        <Show when={props.dim > 0}>
          <div
            style={{
              position: "absolute",
              top: 0,
              left: 0,
              right: 0,
              bottom: 0,
              pointerEvents: "none",
              backgroundColor: `${props.theme.background}${alpha(props.dim)}`,
            }}
          />
        </Show>
        {props.children}
      </div>
    </div>
  );
}

function alpha(amount: number): string {
  return Math.round(Math.min(1, Math.max(0, amount)) * 255)
    .toString(16)
    .padStart(2, "0");
}

/** POSIX shell quoting for dropped paths, so a drop is text, never a command. */
export function quotePaths(paths: readonly string[]): string {
  return paths
    .map((path) => (/^[\w@%+=:,./-]+$/.test(path) ? path : `'${path.replaceAll("'", "'\\''")}'`))
    .join(" ");
}
