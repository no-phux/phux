import type { JSX } from "solid-js";
import { plainKey, type KeyLike } from "../shell/keymap";
import { Icon, IconButton, Label, row, usePalette } from "../ui/controls";
import { radius, uiFont } from "../ui/theme";

/**
 * Floating find field over the focused pane. Search runs in the engine's
 * document model through the view, so each view keeps its own matches.
 */
export function FindBar(props: {
  query: string;
  status: string;
  caseSensitive: boolean;
  setQuery: (query: string) => void;
  step: (delta: 1 | -1) => void;
  toggleCase: () => void;
  close: () => void;
}): JSX.Element {
  const colors = usePalette();
  function keyDown(event: KeyLike): void {
    if (plainKey(event) === "escape") props.close();
  }
  return (
    <div
      style={row({
        position: "absolute",
        top: 8,
        right: 14,
        width: 360,
        height: 36,
        gap: 6,
        paddingLeft: 10,
        paddingRight: 4,
        borderRadius: radius.control,
        borderWidth: 1,
        borderColor: colors().border,
        backgroundColor: colors().raised,
        boxShadow: { offsetX: 0, offsetY: 8, blurRadius: 24, spreadRadius: 0, color: "#00000066" },
      })}
    >
      <Icon name="search" size={13} color={colors().muted} />
      <input
        autoFocus
        value={props.query}
        placeholder="Find in terminal"
        onChange={(event) => props.setQuery(event.value ?? "")}
        onKeyDown={keyDown}
        // A single-line input turns Enter into `submit` (and Shift-Enter into
        // a newline it drops); keyDown never sees either. ⇧⌘G steps back.
        onSubmit={() => props.step(1)}
        style={{
          flexGrow: 1,
          height: 26,
          fontSize: uiFont.size,
          color: colors().foreground,
          backgroundColor: "transparent",
          borderWidth: 0,
        }}
      />
      <Label size={uiFont.small} color={colors().muted}>
        {props.status}
      </Label>
      <div
        role="button"
        aria-label="Match case"
        onClick={() => props.toggleCase()}
        style={row({
          height: 22,
          paddingLeft: 5,
          paddingRight: 5,
          borderRadius: 4,
          cursor: "pointer",
          backgroundColor: props.caseSensitive ? colors().accentWash : "transparent",
          hover: { backgroundColor: colors().hover },
        })}
      >
        <text
          style={{
            fontSize: 11,
            fontWeight: 700,
            color: props.caseSensitive ? colors().accent : colors().muted,
          }}
        >
          Aa
        </text>
      </div>
      <IconButton icon="chevronUp" label="Previous match" run={() => props.step(-1)} size={22} />
      <IconButton icon="chevronDown" label="Next match" run={() => props.step(1)} size={22} />
      <IconButton icon="close" label="Close find" run={() => props.close()} size={22} />
    </div>
  );
}
