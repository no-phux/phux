import { createMemo, createSignal, For, Show, type Accessor, type JSX } from "solid-js";
import { displayPath, type PathRow, type PickerState } from "../path-picker";
import { Icon, Label, column, row, usePalette } from "../ui/controls";
import { radius, uiFont } from "../ui/theme";
import { plainKey, type KeyLike } from "./keymap";
import { Overlay } from "./palette";

const VISIBLE = 11;
const ROW_HEIGHT = 34;

/** One selectable line: the parent directory, or a host path. */
interface Line {
  path: string;
  kind: string;
  parent: boolean;
}

/**
 * Insert Path: browse or fuzzy-search paths on the host that runs the
 * focused terminal (`PATH_QUERY`), never this Mac's disk. An empty field
 * browses the current directory; typing searches beneath it. Enter inserts
 * the selected path as one shell-quoted word and presses nothing else; Tab
 * or → opens a directory.
 */
export function PathPicker(props: {
  state: PickerState;
  search: (query: string) => void;
  open: (directory: string) => void;
  insert: (path: string) => void;
  close: () => void;
}): JSX.Element {
  const colors = usePalette();
  const [selected, setSelected] = createSignal(0);
  const [offset, setOffset] = createSignal(0);
  const lines = createMemo<Line[]>(() => {
    const rows = props.state.rows.map((item: PathRow) => ({ ...item, parent: false }));
    const parent = props.state.parent;
    return parent !== undefined && props.state.query === ""
      ? [{ path: parent, kind: "directory", parent: true }, ...rows]
      : rows;
  });
  const visible = createMemo(() => lines().slice(offset(), offset() + VISIBLE));

  function select(index: number): void {
    const count = lines().length;
    if (count === 0) return;
    const next = (index + count) % count;
    setSelected(next);
    if (next < offset()) setOffset(next);
    else if (next >= offset() + VISIBLE) setOffset(next - VISIBLE + 1);
  }

  function reset(): void {
    setSelected(0);
    setOffset(0);
  }

  function open(line: Line | undefined): void {
    if (!line || line.kind === "file") return;
    reset();
    props.open(line.path);
  }

  function commit(line: Line | undefined): void {
    if (!line) return;
    if (line.parent) open(line);
    else props.insert(line.path);
  }

  function keyDown(event: KeyLike): void {
    const key = plainKey(event);
    if (key === "escape") props.close();
    else if (key === "down") select(selected() + 1);
    else if (key === "up") select(selected() - 1);
    else if (key === "tab" || key === "right") open(lines()[selected()]);
  }

  return (
    <Overlay close={props.close} width={680} top={72}>
      <div style={row({ gap: 10, paddingLeft: 16, paddingRight: 16, height: 48 })}>
        <Icon name="folder" size={15} color={colors().muted} />
        <input
          autoFocus
          value={props.state.query}
          placeholder={`Search in ${displayPath(props.state.root)}…`}
          onChange={(event) => {
            reset();
            props.search(event.value ?? "");
          }}
          onKeyDown={keyDown}
          // A single-line input turns Enter into `submit`; keyDown never sees it.
          onSubmit={() => commit(lines()[selected()])}
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
          {(line, index): JSX.Element => {
            const absolute = (): number => index() + offset();
            const active = (): boolean => absolute() === selected();
            return (
              <div
                onClick={() => commit(line)}
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
                <Icon
                  name={line.parent ? "chevronUp" : line.kind === "file" ? "copy" : "folder"}
                  size={14}
                  color={active() ? colors().accent : colors().muted}
                />
                <Label mono grow weight={active() ? 600 : 400}>
                  {line.parent ? "..  (parent)" : displayPath(line.path)}
                </Label>
                <Show when={!line.parent && line.kind !== "file"}>
                  <div
                    onClick={() => open(line)}
                    style={row({ width: 20, justifyContent: "center", cursor: "pointer" })}
                  >
                    <Icon name="chevronRight" size={14} color={colors().muted} />
                  </div>
                </Show>
              </div>
            );
          }}
        </For>
        <Show when={lines().length === 0}>
          <div style={row({ height: 56, justifyContent: "center" })}>
            <Label color={colors().muted}>
              {props.state.pending !== undefined ? "Searching host paths…" : "No paths."}
            </Label>
          </div>
        </Show>
      </div>
      <div style={{ height: 1, backgroundColor: colors().border }} />
      <div style={row({ gap: 14, height: 30, paddingLeft: 16, paddingRight: 16 })}>
        <Show when={props.state.message}>
          {(message: Accessor<string>): JSX.Element => (
            <Label size={uiFont.small} color={colors().muted}>
              {displayPath(message())}
            </Label>
          )}
        </Show>
        <div style={{ flexGrow: 1 }} />
        <Label size={uiFont.small} color={colors().faint}>
          {"↩ insert   tab open folder   esc close"}
        </Label>
      </div>
    </Overlay>
  );
}
