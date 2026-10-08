import { createSignal, For, Show, type JSX, type Accessor } from "solid-js";
import type { DesktopServerInfo } from "../../native/generated/index";
import { displayChord, plainKey } from "../shell/keymap";
import { Overlay } from "../shell/palette";
import { Button, IconButton, Kbd, Label, column, row, usePalette } from "../ui/controls";
import type { IconName } from "../ui/icons";
import { Icon } from "../ui/controls";
import { palette, radius, themeById, uiFont, type Theme } from "../ui/theme";
import { defaultDisplay, fontFamilies, type DisplayPrefs } from "../workspace/persist";

export interface ShortcutRow {
  title: string;
  chord: string;
  group: string;
}

type Section = "appearance" | "terminal" | "keyboard" | "ghostty" | "connection";

const SECTIONS: { id: Section; label: string; icon: IconName }[] = [
  { id: "appearance", label: "Appearance", icon: "palette" },
  { id: "terminal", label: "Terminal", icon: "terminal" },
  { id: "keyboard", label: "Keyboard", icon: "command" },
  { id: "ghostty", label: "Ghostty", icon: "copy" },
  { id: "connection", label: "Connection", icon: "bolt" },
];

/** What Settings shows and can do about the user's Ghostty config. */
export interface GhosttyPanel {
  found: boolean;
  unmapped: string[];
  font: string | undefined;
  apply: () => void;
  reload: () => void;
}

/**
 * Every value shows its effect immediately and persists with the layout
 * snapshot. Nothing here reaches the server except Reconnect.
 */
export function Settings(props: {
  prefs: DisplayPrefs;
  update: (change: Partial<DisplayPrefs>) => void;
  shortcuts: ShortcutRow[];
  themes: Theme[];
  ghostty: GhosttyPanel;
  server: DesktopServerInfo | undefined;
  socket: string;
  session: string;
  status: string;
  reconnect: () => void;
  close: () => void;
}): JSX.Element {
  const colors = usePalette();
  const [section, setSection] = createSignal<Section>("appearance");
  return (
    <Overlay close={props.close} width={760} top={64}>
      <div
        style={row({ alignItems: "stretch", height: 560 })}
        onKeyDown={(event) => {
          if (plainKey(event) === "escape") props.close();
        }}
      >
        <div
          style={column({
            width: 190,
            padding: 10,
            gap: 2,
            backgroundColor: colors().background,
            borderRightWidth: 1,
            borderColor: colors().border,
          })}
        >
          <div style={{ paddingLeft: 10, paddingTop: 6, paddingBottom: 10 }}>
            <Label weight={700} size={uiFont.title}>
              Settings
            </Label>
          </div>
          <For each={SECTIONS}>
            {(item): JSX.Element => (
              <div
                onClick={() => setSection(item.id)}
                style={row({
                  gap: 9,
                  height: 32,
                  paddingLeft: 10,
                  borderRadius: radius.control,
                  cursor: "pointer",
                  backgroundColor: section() === item.id ? colors().active : "transparent",
                  hover: { backgroundColor: colors().hover },
                })}
              >
                <Icon
                  name={item.icon}
                  size={14}
                  color={section() === item.id ? colors().accent : colors().muted}
                />
                <Label weight={section() === item.id ? 600 : 500}>{item.label}</Label>
              </div>
            )}
          </For>
        </div>
        <div style={column({ flexGrow: 1, padding: 22, gap: 18, overflowY: "scroll" })}>
          <Show when={section() === "appearance"}>
            <Group
              title="Theme"
              hint="Chrome and terminal default colours. Applications' own colours still win."
            >
              <div style={{ display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 10 }}>
                <For each={props.themes}>
                  {(theme): JSX.Element => (
                    <ThemeCard
                      theme={theme}
                      selected={props.prefs.themeId === theme.id}
                      pick={() => props.update({ themeId: theme.id })}
                    />
                  )}
                </For>
              </div>
            </Group>
            <Group title="Layout">
              <Toggle
                label="Show sidebar"
                hint="⌘B"
                value={props.prefs.sidebarVisible}
                set={(sidebarVisible) => props.update({ sidebarVisible })}
              />
              <Toggle
                label="Agent notifications"
                hint="Toast when an agent finishes or needs you"
                value={props.prefs.notifications}
                set={(notifications) => props.update({ notifications })}
              />
            </Group>
          </Show>
          <Show when={section() === "terminal"}>
            <Group title="Font family">
              <div style={{ display: "flex", flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
                <For
                  each={[
                    ...new Set([props.ghostty.font, props.prefs.fontFamily, ...fontFamilies]),
                  ].flatMap((family) => (family ? [family] : []))}
                >
                  {(family): JSX.Element => (
                    <Chip
                      label={family}
                      font={family}
                      selected={props.prefs.fontFamily === family}
                      pick={() => props.update({ fontFamily: family })}
                    />
                  )}
                </For>
              </div>
            </Group>
            <Group title="Size">
              <Stepper
                label="Font size"
                value={`${props.prefs.fontSize} pt`}
                less={() => props.update({ fontSize: props.prefs.fontSize - 1 })}
                more={() => props.update({ fontSize: props.prefs.fontSize + 1 })}
                reset={() => props.update({ fontSize: defaultDisplay.fontSize })}
              />
              <Stepper
                label="Line height"
                value={props.prefs.lineHeight.toFixed(2)}
                less={() => props.update({ lineHeight: props.prefs.lineHeight - 0.05 })}
                more={() => props.update({ lineHeight: props.prefs.lineHeight + 0.05 })}
                reset={() => props.update({ lineHeight: defaultDisplay.lineHeight })}
              />
              <Stepper
                label="Cell width"
                value={`${Math.round(props.prefs.cellWidth * 100)}%`}
                less={() => props.update({ cellWidth: props.prefs.cellWidth - 0.01 })}
                more={() => props.update({ cellWidth: props.prefs.cellWidth + 0.01 })}
                reset={() => props.update({ cellWidth: 1 })}
              />
              <Stepper
                label="Padding"
                value={`${props.prefs.paddingX} \u00d7 ${props.prefs.paddingY}`}
                less={() =>
                  props.update({
                    paddingX: props.prefs.paddingX - 1,
                    paddingY: props.prefs.paddingY - 1,
                  })
                }
                more={() =>
                  props.update({
                    paddingX: props.prefs.paddingX + 1,
                    paddingY: props.prefs.paddingY + 1,
                  })
                }
                reset={() =>
                  props.update({
                    paddingX: defaultDisplay.paddingX,
                    paddingY: defaultDisplay.paddingY,
                  })
                }
              />
              <Stepper
                label="Unfocused pane opacity"
                value={`${Math.round(props.prefs.unfocusedOpacity * 100)}%`}
                less={() => props.update({ unfocusedOpacity: props.prefs.unfocusedOpacity - 0.05 })}
                more={() => props.update({ unfocusedOpacity: props.prefs.unfocusedOpacity + 0.05 })}
                reset={() => props.update({ unfocusedOpacity: 1 })}
              />
            </Group>
            <Group title="Input">
              <Toggle
                label="Option key sends Alt"
                hint="Off keeps macOS Option characters such as å and ß"
                value={props.prefs.optionAsAlt}
                set={(optionAsAlt) => props.update({ optionAsAlt })}
              />
            </Group>
            <Group title="Preview">
              <div
                style={column({
                  padding: 14,
                  gap: 2,
                  borderRadius: radius.control,
                  backgroundColor: palette(themeById(props.prefs.themeId, props.themes)).background,
                  borderWidth: 1,
                  borderColor: colors().border,
                })}
              >
                <text
                  style={{
                    fontFamily: props.prefs.fontFamily,
                    fontSize: props.prefs.fontSize,
                    lineHeight: props.prefs.fontSize * props.prefs.lineHeight,
                    color: palette(themeById(props.prefs.themeId, props.themes)).foreground,
                  }}
                >
                  {
                    "~/src/phux ❯ cargo nextest run\n    Finished test [unoptimized] in 0.42s\n     PASS [ 0.01s] phux-core layout::split"
                  }
                </text>
              </div>
            </Group>
          </Show>
          <Show when={section() === "keyboard"}>
            <Group
              title="Shortcuts"
              hint="Command-key chords and your Ghostty keybinds belong to the app; everything else goes to the terminal."
            >
              <div style={column({ gap: 1 })}>
                <For each={props.shortcuts}>
                  {(shortcut): JSX.Element => (
                    <div style={row({ height: 28, gap: 10, paddingLeft: 4, paddingRight: 4 })}>
                      <Label grow>{shortcut.title}</Label>
                      <Label size={uiFont.small} color={colors().faint}>
                        {shortcut.group}
                      </Label>
                      <Kbd>{displayChord(shortcut.chord)}</Kbd>
                    </div>
                  )}
                </For>
              </div>
            </Group>
          </Show>
          <Show when={section() === "ghostty"}>
            <Group
              title="Ghostty config"
              hint={
                props.ghostty.found
                  ? "Font, cell size, colours, palette, padding, dimming and keybinds are read from your Ghostty config."
                  : "No Ghostty config found in ~/.config/ghostty or Application Support."
              }
            >
              <div style={row({ gap: 8 })}>
                <Button
                  label="Use Ghostty look"
                  icon="palette"
                  tone="accent"
                  run={() => props.ghostty.apply()}
                />
                <Button label="Reload config" icon="refresh" run={() => props.ghostty.reload()} />
              </div>
              <Toggle
                label="Use Ghostty keybinds"
                hint="Your keybind lines override the built-in chords"
                value={props.prefs.ghosttyKeys}
                set={(ghosttyKeys) => props.update({ ghosttyKeys })}
              />
            </Group>
            <Show when={props.ghostty.unmapped.length > 0}>
              <Group
                title="Not supported here"
                hint="These keybinds (key sequences, bare letters, or actions with no equivalent yet) are skipped; their keys keep the built-in behaviour."
              >
                <div style={column({ gap: 2 })}>
                  <For each={props.ghostty.unmapped}>
                    {(line): JSX.Element => (
                      <Label size={uiFont.small} color={colors().muted} mono>
                        {line}
                      </Label>
                    )}
                  </For>
                </div>
              </Group>
            </Show>
          </Show>
          <Show when={section() === "connection"}>
            <Group title="Server">
              <Fact label="Status" value={props.status} />
              <Fact label="Socket" value={props.socket} mono />
              <Fact label="Home session" value={props.session} />
              <Fact label="Server id" value={props.server?.serverId ?? "—"} mono />
              <Fact label="Protocol" value={props.server?.protocol ?? "—"} mono />
              <Fact label="Connection epoch" value={props.server?.connectionEpoch ?? "—"} mono />
              <Fact label="Capabilities" value={props.server?.features.join(", ") ?? "—"} />
            </Group>
            <div style={row({ gap: 8 })}>
              <Button
                label="Reconnect"
                icon="refresh"
                tone="accent"
                run={() => props.reconnect()}
              />
            </div>
          </Show>
        </div>
        <div style={{ position: "absolute", top: 10, right: 10 }}>
          <IconButton icon="close" label="Close settings" run={() => props.close()} />
        </div>
      </div>
    </Overlay>
  );
}

function Group(props: { title: string; hint?: string; children: JSX.Element }): JSX.Element {
  const colors = usePalette();
  return (
    <div style={column({ gap: 10 })}>
      <div style={column({ gap: 3 })}>
        <Label weight={700}>{props.title}</Label>
        <Show when={props.hint}>
          {(hint: Accessor<string>): JSX.Element => (
            <Label size={uiFont.small} color={colors().muted}>
              {hint()}
            </Label>
          )}
        </Show>
      </div>
      {props.children}
    </div>
  );
}

function ThemeCard(props: { theme: Theme; selected: boolean; pick: () => void }): JSX.Element {
  const colors = usePalette();
  const tokens = (): ReturnType<typeof palette> => palette(props.theme);
  return (
    <div
      onClick={() => props.pick()}
      style={column({
        width: 162,
        gap: 8,
        padding: 8,
        borderRadius: radius.control + 2,
        cursor: "pointer",
        borderWidth: props.selected ? 2 : 1,
        borderColor: props.selected ? colors().accent : colors().border,
        hover: { borderColor: colors().accent },
      })}
    >
      <div
        style={column({
          height: 64,
          padding: 8,
          gap: 5,
          borderRadius: 6,
          backgroundColor: tokens().background,
        })}
      >
        <div style={row({ gap: 4 })}>
          <Swatch color={tokens().accent} />
          <Swatch color={tokens().success} />
          <Swatch color={tokens().warning} />
          <Swatch color={tokens().danger} />
        </div>
        <div
          style={{ height: 5, width: 110, borderRadius: 3, backgroundColor: tokens().foreground }}
        />
        <div style={{ height: 5, width: 72, borderRadius: 3, backgroundColor: tokens().muted }} />
      </div>
      <Label weight={props.selected ? 600 : 500}>{props.theme.name}</Label>
    </div>
  );
}

function Swatch(props: { color: string }): JSX.Element {
  return <div style={{ width: 10, height: 10, borderRadius: 5, backgroundColor: props.color }} />;
}

function Chip(props: {
  label: string;
  font: string;
  selected: boolean;
  pick: () => void;
}): JSX.Element {
  const colors = usePalette();
  return (
    <div
      onClick={() => props.pick()}
      style={row({
        height: 30,
        paddingLeft: 12,
        paddingRight: 12,
        borderRadius: radius.control,
        cursor: "pointer",
        borderWidth: 1,
        borderColor: props.selected ? colors().accent : colors().border,
        backgroundColor: props.selected ? colors().accentWash : "transparent",
        hover: { backgroundColor: colors().hover },
      })}
    >
      <text style={{ fontFamily: props.font, fontSize: 12.5, color: colors().foreground }}>
        {props.label}
      </text>
    </div>
  );
}

function Toggle(props: {
  label: string;
  hint?: string;
  value: boolean;
  set: (value: boolean) => void;
}): JSX.Element {
  const colors = usePalette();
  return (
    <div
      onClick={() => props.set(!props.value)}
      style={row({ gap: 12, minHeight: 36, cursor: "pointer" })}
    >
      <div style={column({ flexGrow: 1, gap: 2 })}>
        <Label weight={500}>{props.label}</Label>
        <Show when={props.hint}>
          {(hint: Accessor<string>): JSX.Element => (
            <Label size={uiFont.small} color={colors().muted}>
              {hint()}
            </Label>
          )}
        </Show>
      </div>
      <div
        style={row({
          width: 34,
          height: 20,
          padding: 2,
          borderRadius: 10,
          justifyContent: props.value ? "flex-end" : "flex-start",
          backgroundColor: props.value ? colors().accent : colors().active,
        })}
      >
        <div style={{ width: 16, height: 16, borderRadius: 8, backgroundColor: "#ffffff" }} />
      </div>
    </div>
  );
}

function Stepper(props: {
  label: string;
  value: string;
  less: () => void;
  more: () => void;
  reset: () => void;
}): JSX.Element {
  const colors = usePalette();
  return (
    <div style={row({ gap: 8, height: 34 })}>
      <Label grow weight={500}>
        {props.label}
      </Label>
      <div
        style={row({
          gap: 2,
          padding: 2,
          borderRadius: radius.control,
          borderWidth: 1,
          borderColor: colors().border,
        })}
      >
        <IconButton
          icon="chevronDown"
          label={`Decrease ${props.label}`}
          run={() => props.less()}
          size={24}
        />
        <div style={row({ width: 64, justifyContent: "center" })}>
          <Label mono>{props.value}</Label>
        </div>
        <IconButton
          icon="chevronUp"
          label={`Increase ${props.label}`}
          run={() => props.more()}
          size={24}
        />
      </div>
      <IconButton
        icon="refresh"
        label={`Reset ${props.label}`}
        run={() => props.reset()}
        size={24}
      />
    </div>
  );
}

function Fact(props: { label: string; value: string; mono?: boolean }): JSX.Element {
  const colors = usePalette();
  return (
    <div style={row({ gap: 12, minHeight: 26, alignItems: "flex-start" })}>
      <div style={{ width: 140, flexShrink: 0 }}>
        <Label color={colors().muted}>{props.label}</Label>
      </div>
      <text
        style={{
          flexGrow: 1,
          flexShrink: 1,
          fontSize: uiFont.size,
          color: colors().foreground,
          ...(props.mono ? { fontFamily: "Paper Mono" } : {}),
        }}
      >
        {props.value}
      </text>
    </div>
  );
}
