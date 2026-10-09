import { Show, createSignal, type JSX, type Accessor } from "solid-js";
import type { DesktopPane } from "../../native/generated/index";
import type { AgentInfo } from "../bridge/desktop";
import type { HostAction, TerminalTheme } from "../terminal-element";
import { IconButton, Label, Pill, row, usePalette } from "../ui/controls";
import { agentColors } from "../ui/theme";
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
  /** The terminal's paint revision: changes when a wake can repaint it. */
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
  // Rex has no per-pane title bar, even when split. `showHeader` now only
  // means "this placement is in a split" so focus can be a hairline, not a card.
  const [hot, setHot] = createSignal(false);
  const split = (): boolean => props.showHeader;
  return (
    <div
      onMouseEnter={() => setHot(true)}
      onMouseLeave={() => setHot(false)}
      style={{
        display: "flex",
        flexDirection: "column",
        flexGrow: 1,
        minWidth: 0,
        minHeight: 0,
        position: "relative",
        backgroundColor: props.theme.background,
        borderWidth: 0,
        // Focus is an inset hairline on the focused split only. A lone pane
        // stays full-bleed, matching Rex.
        boxShadow:
          split() && props.selected ? `inset 0 0 0 1px ${colors().accent}99` : "none",
        overflow: "hidden",
      }}
    >
      <Show when={hot()}>
        <div
          style={row({
            position: "absolute",
            top: 6,
            right: 6,
            zIndex: 2,
            height: 24,
            gap: 2,
            paddingLeft: 6,
            paddingRight: 2,
            borderRadius: 6,
            backgroundColor: `${colors().raised}ee`,
            borderWidth: 1,
            borderColor: colors().border,
          })}
        >
          <Show when={props.agent}>
            {(agent: Accessor<AgentInfo>): JSX.Element => (
              <Pill color={tint()} subtle>
                {agent().state}
              </Pill>
            )}
          </Show>
          <Show when={props.fenced}>
            <Pill color={colors().warning}>delivery unknown</Pill>
          </Show>
          <IconButton icon="splitRight" label="Split right" run={() => props.splitRight()} size={22} />
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
        <Show when={!props.placement.viewId}>
          <Label color={colors().muted}>Waiting for terminal…</Label>
        </Show>
        <Show when={props.placement.viewId}>
          {(viewId: Accessor<string>): JSX.Element => (
            <phux-terminal
              clientHandle={props.clientHandle}
              terminalId={props.placement.terminalId}
              viewId={viewId()}
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
          )}
        </Show>
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
