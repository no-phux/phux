import { createMemo, createSignal, For, Show, type JSX, type Accessor } from "solid-js";
import { Icon, Kbd, Label, StatusDot, column, row, usePalette } from "../ui/controls";
import type { IconName } from "../ui/icons";
import { radius, uiFont } from "../ui/theme";
import { rank } from "./filter";
import { displayChord, plainKey, type KeyLike } from "./keymap";

export interface PaletteItem {
  id: string;
  title: string;
  detail?: string;
  group?: string;
  chord?: string;
  icon?: IconName;
  agentState?: string;
  run: () => void;
}

const VISIBLE = 11;
const ROW_HEIGHT = 40;

/**
 * Centered overlay: one search field, a ranked list, keyboard-first. The
 * selection resets on every edit; Up/Down wrap; Enter runs; Escape closes.
 */
export function CommandPalette(props: {
  placeholder: string;
  items: PaletteItem[];
  close: () => void;
  initialQuery?: string;
  /** Called on edit so a caller can switch modes (a leading ">" means commands). */
  onQuery?: (query: string) => boolean;
}): JSX.Element {
  const colors = usePalette();
  const [query, setQuery] = createSignal(props.initialQuery ?? "");
  const [selected, setSelected] = createSignal(0);
  const [offset, setOffset] = createSignal(0);
  const results = createMemo(() =>
    rank(props.items, query(), (item) => `${item.title} ${item.detail ?? ""} ${item.group ?? ""}`),
  );
  const visible = createMemo(() => results().slice(offset(), offset() + VISIBLE));

  function select(index: number): void {
    const count = results().length;
    if (count === 0) return;
    const next = (index + count) % count;
    setSelected(next);
    if (next < offset()) setOffset(next);
    else if (next >= offset() + VISIBLE) setOffset(next - VISIBLE + 1);
  }

  function run(item: PaletteItem | undefined): void {
    if (!item) return;
    props.close();
    item.run();
  }

  function keyDown(event: KeyLike): void {
    const key = plainKey(event);
    if (key === "escape") props.close();
    else if (key === "down") select(selected() + 1);
    else if (key === "up") select(selected() - 1);
  }

  return (
    <Overlay close={props.close} width={620} top={72}>
      <div style={row({ gap: 10, paddingLeft: 16, paddingRight: 16, height: 48 })}>
        <Icon name="search" size={15} color={colors().muted} />
        <input
          autoFocus
          value={query()}
          placeholder={props.placeholder}
          onChange={(event) => {
            const value = event.value ?? "";
            if (props.onQuery?.(value)) return;
            setQuery(value);
            setSelected(0);
            setOffset(0);
          }}
          onKeyDown={keyDown}
          // A single-line input turns Enter into `submit`; keyDown never sees it.
          onSubmit={() => run(results()[selected()])}
          style={{
            flexGrow: 1,
            height: 30,
            fontSize: 14,
            color: colors().foreground,
            backgroundColor: "transparent",
            borderWidth: 0,
          }}
        />
      </div>
      <div style={{ height: 1, backgroundColor: colors().border }} />
      <div style={column({ padding: 6, gap: 1 })}>
        <For each={visible()}>
          {(item, index): JSX.Element => {
            const absolute = (): number => index() + offset();
            const active = (): boolean => absolute() === selected();
            return (
              <div
                onClick={() => run(item)}
                onMouseEnter={() => setSelected(absolute())}
                style={row({
                  gap: 10,
                  height: ROW_HEIGHT,
                  paddingLeft: 10,
                  paddingRight: 10,
                  borderRadius: radius.control,
                  cursor: "pointer",
                  backgroundColor: active() ? colors().accentWash : "transparent",
                })}
              >
                <Show when={!item.agentState}>
                  <Icon
                    name={item.icon ?? "command"}
                    size={14}
                    color={active() ? colors().accent : colors().muted}
                  />
                </Show>
                <Show when={item.agentState}>
                  {(state: Accessor<string>): JSX.Element => (
                    <div style={row({ width: 14, justifyContent: "center" })}>
                      <StatusDot state={state()} />
                    </div>
                  )}
                </Show>
                <div style={column({ flexGrow: 1, flexShrink: 1, gap: 1, overflow: "hidden" })}>
                  <Label weight={active() ? 600 : 500}>{item.title}</Label>
                  <Show when={item.detail}>
                    {(detail: Accessor<string>): JSX.Element => (
                      <Label size={uiFont.small} color={colors().muted}>
                        {detail()}
                      </Label>
                    )}
                  </Show>
                </div>
                <Show when={item.group}>
                  {(group: Accessor<string>): JSX.Element => (
                    <Label size={uiFont.small} color={colors().faint}>
                      {group()}
                    </Label>
                  )}
                </Show>
                <Show when={item.chord}>
                  {(chord: Accessor<string>): JSX.Element => <Kbd>{displayChord(chord())}</Kbd>}
                </Show>
              </div>
            );
          }}
        </For>
        <Show when={results().length === 0}>
          <div style={row({ height: 64, justifyContent: "center" })}>
            <Label color={colors().muted}>No matches. Try a shorter search.</Label>
          </div>
        </Show>
      </div>
      <div style={{ height: 1, backgroundColor: colors().border }} />
      <div style={row({ gap: 14, height: 30, paddingLeft: 16, paddingRight: 16 })}>
        <Label size={uiFont.small} color={colors().faint}>
          {`${results().length} results`}
        </Label>
        <div style={{ flexGrow: 1 }} />
        <Label size={uiFont.small} color={colors().faint}>
          {"↑↓ navigate   ↩ open   esc close"}
        </Label>
      </div>
    </Overlay>
  );
}

/**
 * Dimmed backdrop plus a floating panel; a press outside the panel closes it.
 * GPUIX clicks bubble without stopping, so the panel, not the backdrop, owns
 * the outside test.
 */
export function Overlay(props: {
  children: JSX.Element;
  close: () => void;
  width: number;
  top?: number;
}): JSX.Element {
  const colors = usePalette();
  return (
    <div
      style={{
        position: "absolute",
        top: 0,
        left: 0,
        right: 0,
        bottom: 0,
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        paddingTop: props.top ?? 96,
        backgroundColor: colors().backdrop,
      }}
    >
      <div
        onMouseDownOutside={() => props.close()}
        style={column({
          width: props.width,
          maxHeight: 640,
          overflow: "hidden",
          borderRadius: radius.panel,
          borderWidth: 1,
          borderColor: colors().border,
          backgroundColor: colors().surface,
          boxShadow: {
            offsetX: 0,
            offsetY: 18,
            blurRadius: 48,
            spreadRadius: 0,
            color: "#00000080",
          },
        })}
      >
        {props.children}
      </div>
    </div>
  );
}
