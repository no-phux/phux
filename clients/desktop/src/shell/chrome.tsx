import { createSignal, For, Show, type JSX, type Accessor } from "solid-js";
import { plainKey, type KeyLike } from "./keymap";
import {
  Button,
  Icon,
  IconButton,
  Kbd,
  Label,
  StatusDot,
  column,
  row,
  usePalette,
} from "../ui/controls";
import type { IconName } from "../ui/icons";
import { agentColors, radius, uiFont } from "../ui/theme";
import { Overlay } from "./palette";

// ── Status bar ─────────────────────────────────────────────────────

export interface StatusInfo {
  connection: string;
  error: string | undefined;
  session: string;
  socket: string;
  geometry: string | undefined;
  scrollback: string | undefined;
  fenced: boolean;
  pending: number;
  agents: Record<string, number>;
  theme: string;
  fontSize: number;
}

/**
 * Field-by-field equality for the status memo. It is recomputed on every
 * wake; GPUIX re-sends every style a changed object reaches, and any sent
 * style redraws the whole window, so an unchanged status must not propagate.
 */
export function sameStatus(a: StatusInfo, b: StatusInfo): boolean {
  const agents = Object.keys(a.agents);
  return (
    a.connection === b.connection &&
    a.error === b.error &&
    a.session === b.session &&
    a.socket === b.socket &&
    a.geometry === b.geometry &&
    a.scrollback === b.scrollback &&
    a.fenced === b.fenced &&
    a.pending === b.pending &&
    a.theme === b.theme &&
    a.fontSize === b.fontSize &&
    agents.length === Object.keys(b.agents).length &&
    agents.every((state) => a.agents[state] === b.agents[state])
  );
}

export function StatusBar(props: {
  info: StatusInfo;
  follow: () => void;
  reconnect: () => void;
  openSettings: () => void;
}): JSX.Element {
  const colors = usePalette();
  const healthy = (): boolean => props.info.connection === "Attached";
  const tone = (): string =>
    healthy()
      ? colors().success
      : props.info.connection === "Connecting"
        ? colors().warning
        : colors().danger;
  return (
    <div
      style={row({
        height: 26,
        flexShrink: 0,
        gap: 12,
        paddingLeft: 12,
        paddingRight: 8,
        backgroundColor: colors().surface,
        borderTopWidth: 1,
        borderColor: colors().border,
      })}
    >
      <div
        onClick={() => {
          if (!healthy()) props.reconnect();
        }}
        style={row({ gap: 6, cursor: healthy() ? "default" : "pointer" })}
      >
        <div style={{ width: 7, height: 7, borderRadius: 4, backgroundColor: tone() }} />
        <Label size={uiFont.small} color={colors().subtext}>
          {healthy() ? props.info.session : connectionText(props.info.connection)}
        </Label>
      </div>
      <Show when={!healthy() && props.info.error}>
        {(error: Accessor<string>): JSX.Element => (
          <Label size={uiFont.small} color={colors().danger}>
            {error()}
          </Label>
        )}
      </Show>
      <Show when={props.info.pending > 0}>
        <Label size={uiFont.small} color={colors().muted}>
          {"starting terminal…"}
        </Label>
      </Show>
      <Show when={props.info.fenced}>
        <div style={row({ gap: 5 })}>
          <Icon name="bolt" size={12} color={colors().warning} />
          <Label size={uiFont.small} color={colors().warning}>
            input delivery unknown: waiting for fresh output
          </Label>
        </div>
      </Show>
      <div style={{ flexGrow: 1 }} />
      <For each={Object.entries(props.info.agents).filter(([, count]) => count > 0)}>
        {([state, count]): JSX.Element => (
          <div style={row({ gap: 5 })}>
            <StatusDot state={state} size={7} />
            <Label size={uiFont.small} color={agentColors[state] ?? colors().muted}>
              {`${count} ${state}`}
            </Label>
          </div>
        )}
      </For>
      <Show when={props.info.scrollback}>
        {(scroll: Accessor<string>): JSX.Element => (
          <div
            onClick={() => props.follow()}
            style={row({
              gap: 5,
              cursor: "pointer",
              paddingLeft: 6,
              paddingRight: 6,
              height: 20,
              borderRadius: 4,
              backgroundColor: colors().accentWash,
            })}
          >
            <Icon name="follow" size={11} color={colors().accent} />
            <Label size={uiFont.small} color={colors().accent}>
              {scroll()}
            </Label>
          </div>
        )}
      </Show>
      <Show when={props.info.geometry}>
        {(geometry: Accessor<string>): JSX.Element => (
          <Label size={uiFont.small} color={colors().muted} mono>
            {geometry()}
          </Label>
        )}
      </Show>
      <div
        onClick={() => props.openSettings()}
        style={row({ gap: 6, cursor: "pointer", hover: { opacity: 0.8 } })}
      >
        <Label size={uiFont.small} color={colors().muted}>
          {`${props.info.theme} · ${props.info.fontSize}pt`}
        </Label>
      </div>
    </div>
  );
}

function connectionText(status: string): string {
  if (status === "Connecting") return "connecting…";
  if (status === "Failed") return "connection failed · click to retry";
  if (status === "Closed") return "disconnected · click to reconnect";
  return status.toLowerCase();
}

// ── Toasts ──────────────────────────────────────────────────────────

export interface Toast {
  id: number;
  kind: "info" | "error" | "agent" | "success";
  title: string;
  body?: string;
  agentState?: string;
  terminalId?: string;
  at: number;
  action?: () => void;
}

export function Toasts(props: { toasts: Toast[]; dismiss: (id: number) => void }): JSX.Element {
  const colors = usePalette();
  const edge = (toast: Toast): string =>
    toast.kind === "error"
      ? colors().danger
      : toast.kind === "success"
        ? colors().success
        : toast.kind === "agent"
          ? (agentColors[toast.agentState ?? "unknown"] ?? colors().accent)
          : colors().accent;
  return (
    <div
      style={column({
        position: "absolute",
        right: 16,
        bottom: 40,
        width: 340,
        gap: 8,
        pointerEvents: "none",
      })}
    >
      <For each={props.toasts}>
        {(toast): JSX.Element => (
          <div
            onClick={() => {
              toast.action?.();
              props.dismiss(toast.id);
            }}
            style={row({
              gap: 10,
              padding: 12,
              alignItems: "flex-start",
              borderRadius: radius.panel,
              borderWidth: 1,
              borderColor: `${edge(toast)}88`,
              backgroundColor: colors().raised,
              cursor: "pointer",
              pointerEvents: "auto",
              boxShadow: {
                offsetX: 0,
                offsetY: 10,
                blurRadius: 30,
                spreadRadius: 0,
                color: "#00000070",
              },
            })}
          >
            <div style={{ paddingTop: 4 }}>
              <Show when={toast.kind !== "agent"}>
                <div
                  style={{ width: 8, height: 8, borderRadius: 4, backgroundColor: edge(toast) }}
                />
              </Show>
              <Show when={toast.kind === "agent"}>
                <StatusDot state={toast.agentState ?? "unknown"} />
              </Show>
            </div>
            <div style={column({ flexGrow: 1, flexShrink: 1, gap: 3, overflow: "hidden" })}>
              <Label weight={600}>{toast.title}</Label>
              <Show when={toast.body}>
                {(body: Accessor<string>): JSX.Element => (
                  <text style={{ color: colors().muted, fontSize: uiFont.small, lineClamp: 3 }}>
                    {body()}
                  </text>
                )}
              </Show>
            </div>
            <IconButton
              icon="close"
              label="Dismiss"
              run={() => props.dismiss(toast.id)}
              size={20}
            />
          </div>
        )}
      </For>
    </div>
  );
}

// ── Dialogs ─────────────────────────────────────────────────────────

export function RenameDialog(props: {
  initial: string;
  apply: (title: string) => void;
  close: () => void;
}): JSX.Element {
  const colors = usePalette();
  const [value, setValue] = createSignal(props.initial);
  function keyDown(event: KeyLike): void {
    const key = plainKey(event);
    if (key === "escape") props.close();
  }
  function submit(): void {
    props.apply(value());
    props.close();
  }
  return (
    <Overlay close={props.close} width={420} top={140}>
      <div style={column({ padding: 18, gap: 12 })}>
        <Label weight={600} size={uiFont.title}>
          Rename tab
        </Label>
        <input
          autoFocus
          value={value()}
          placeholder="Follow the terminal title"
          onChange={(event) => setValue(event.value ?? "")}
          onKeyDown={keyDown}
          // A single-line input turns Enter into `submit`; keyDown never sees it.
          onSubmit={submit}
          style={{
            height: 32,
            paddingLeft: 10,
            fontSize: 13,
            borderRadius: radius.control,
            borderWidth: 1,
            borderColor: colors().border,
            backgroundColor: colors().background,
            color: colors().foreground,
          }}
        />
        <div style={row({ gap: 8, justifyContent: "flex-end" })}>
          <Button label="Cancel" run={() => props.close()} />
          <Button
            label="Rename"
            tone="accent"
            run={() => {
              props.apply(value());
              props.close();
            }}
          />
        </div>
      </div>
    </Overlay>
  );
}

export function ConfirmDialog(props: {
  title: string;
  body: string;
  confirm: string;
  run: () => void;
  close: () => void;
}): JSX.Element {
  const colors = usePalette();
  return (
    <Overlay close={props.close} width={440} top={140}>
      <div
        style={column({ padding: 20, gap: 12 })}
        onKeyDown={(event) => {
          if (plainKey(event) === "escape") props.close();
        }}
      >
        <div style={row({ gap: 10 })}>
          <Icon name="skull" size={18} color={colors().danger} />
          <Label weight={600} size={uiFont.title}>
            {props.title}
          </Label>
        </div>
        <text style={{ fontSize: uiFont.size, lineHeight: 18, color: colors().subtext }}>
          {props.body}
        </text>
        <div style={row({ gap: 8, justifyContent: "flex-end", paddingTop: 6 })}>
          <Button label="Cancel" run={() => props.close()} />
          <Button
            label={props.confirm}
            tone="danger"
            run={() => {
              props.run();
              props.close();
            }}
          />
        </div>
      </div>
    </Overlay>
  );
}

// ── Empty and connection states ─────────────────────────────────────

export function EmptyState(props: {
  status: string;
  error: string | undefined;
  socket: string;
  pending: boolean;
  newTerminal: () => void;
  openFolder: () => void;
  openPalette: () => void;
  reconnect: () => void;
}): JSX.Element {
  const colors = usePalette();
  const connecting = (): boolean => props.status === "Connecting";
  const failed = (): boolean => props.status === "Failed" || props.status === "Closed";
  return (
    <div style={column({ flexGrow: 1, alignItems: "center", justifyContent: "center", gap: 18 })}>
      <text style={{ fontSize: 34, fontWeight: 800, color: colors().foreground }}>phux</text>
      <Show when={connecting()}>
        <Label color={colors().muted}>{`Connecting to ${props.socket}…`}</Label>
      </Show>
      <Show when={failed()}>
        <div style={column({ alignItems: "center", gap: 10, maxWidth: 520 })}>
          <Label color={colors().danger}>{props.error ?? "The connection closed."}</Label>
          <Button label="Reconnect" icon="refresh" tone="accent" run={() => props.reconnect()} />
        </div>
      </Show>
      <Show when={props.status === "Attached"}>
        <div style={column({ width: 340, gap: 6 })}>
          <Action
            icon="terminal"
            label={props.pending ? "Starting terminal…" : "New terminal"}
            chord="⌘T"
            run={props.newTerminal}
          />
          <Action icon="folder" label="Open folder…" chord="⌘O" run={props.openFolder} />
          <Action icon="command" label="Command palette" chord="⇧⌘P" run={props.openPalette} />
        </div>
      </Show>
    </div>
  );
}

function Action(props: {
  icon: IconName;
  label: string;
  chord: string;
  run: () => void;
}): JSX.Element {
  const colors = usePalette();
  return (
    <div
      onClick={() => props.run()}
      style={row({
        gap: 10,
        height: 38,
        paddingLeft: 12,
        paddingRight: 10,
        borderRadius: radius.control,
        cursor: "pointer",
        backgroundColor: colors().surface,
        borderWidth: 1,
        borderColor: colors().border,
        hover: { backgroundColor: colors().hover },
      })}
    >
      <Icon name={props.icon} size={15} color={colors().accent} />
      <Label grow weight={500}>
        {props.label}
      </Label>
      <Kbd>{props.chord}</Kbd>
    </div>
  );
}
