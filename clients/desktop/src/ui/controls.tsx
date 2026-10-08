import { createContext, Show, useContext, type Accessor, type JSX } from "solid-js";
import type { StyleDesc } from "@gpuix/solid";
import { icons, type IconName } from "./icons";
import {
  agentColors,
  defaultThemeId,
  palette,
  radius,
  themeById,
  uiFont,
  type Palette,
} from "./theme";

const fallback = palette(themeById(defaultThemeId));

export const PaletteContext = createContext<Accessor<Palette>>(() => fallback);

export function usePalette(): Accessor<Palette> {
  return useContext(PaletteContext);
}

export function Icon(props: { name: IconName; size?: number; color?: string }): JSX.Element {
  const colors = usePalette();
  return (
    <svg
      source={icons[props.name]}
      style={{
        width: props.size ?? 14,
        height: props.size ?? 14,
        flexShrink: 0,
        color: props.color ?? colors().subtext,
      }}
    />
  );
}

export function Label(props: {
  children: JSX.Element;
  color?: string;
  size?: number;
  weight?: number;
  mono?: boolean;
  grow?: boolean;
  /** Keep the full text and let siblings shrink instead. */
  keep?: boolean;
}): JSX.Element {
  const colors = usePalette();
  return (
    <text
      style={{
        color: props.color ?? colors().foreground,
        fontSize: props.size ?? uiFont.size,
        fontWeight: props.weight ?? 400,
        whiteSpace: "nowrap",
        textOverflow: "ellipsis",
        overflow: "hidden",
        flexShrink: props.keep ? 0 : 1,
        ...(props.grow ? { flexGrow: 1 } : {}),
        ...(props.mono ? { fontFamily: "Paper Mono" } : {}),
      }}
    >
      {props.children}
    </text>
  );
}

/** Square icon button with a quiet hover wash, the default chrome affordance. */
export function IconButton(props: {
  icon: IconName;
  label: string;
  run: () => void;
  size?: number;
  active?: boolean;
  color?: string;
}): JSX.Element {
  const colors = usePalette();
  const box = (): number => props.size ?? 26;
  return (
    <div
      role="button"
      aria-label={props.label}
      onClick={() => props.run()}
      style={{
        width: box(),
        height: box(),
        borderRadius: radius.small + 1,
        alignItems: "center",
        justifyContent: "center",
        display: "flex",
        cursor: "pointer",
        flexShrink: 0,
        backgroundColor: props.active ? colors().active : "transparent",
        hover: { backgroundColor: colors().hover },
        active: { backgroundColor: colors().active },
      }}
    >
      <Icon
        name={props.icon}
        size={Math.round(box() * 0.55)}
        color={props.color ?? colors().subtext}
      />
    </div>
  );
}

export function Button(props: {
  label: string;
  run: () => void;
  tone?: "accent" | "danger" | "plain";
  icon?: IconName;
}): JSX.Element {
  const colors = usePalette();
  const fill = (): string =>
    props.tone === "accent"
      ? colors().accent
      : props.tone === "danger"
        ? colors().danger
        : colors().raised;
  const ink = (): string =>
    props.tone === "accent" || props.tone === "danger" ? colors().accentText : colors().foreground;
  return (
    <div
      role="button"
      aria-label={props.label}
      onClick={() => props.run()}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 6,
        height: 28,
        paddingLeft: 12,
        paddingRight: 12,
        borderRadius: radius.control,
        backgroundColor: fill(),
        borderWidth: 1,
        borderColor: props.tone ? fill() : colors().border,
        cursor: "pointer",
        hover: { opacity: 0.88 },
        active: { opacity: 0.75 },
      }}
    >
      <Show when={props.icon}>
        {(name: Accessor<IconName>): JSX.Element => <Icon name={name()} size={13} color={ink()} />}
      </Show>
      <Label color={ink()} weight={500}>
        {props.label}
      </Label>
    </div>
  );
}

/** Keyboard chord chip, e.g. ⌘⇧P. */
export function Kbd(props: { children: string }): JSX.Element {
  const colors = usePalette();
  return (
    <div
      style={{
        paddingLeft: 5,
        paddingRight: 5,
        height: 18,
        borderRadius: 4,
        alignItems: "center",
        display: "flex",
        backgroundColor: colors().raised,
        borderWidth: 1,
        borderColor: colors().border,
      }}
    >
      <text style={{ color: colors().muted, fontSize: uiFont.small, whiteSpace: "nowrap" }}>
        {props.children}
      </text>
    </div>
  );
}

/**
 * Agent lifecycle dot: filled for working/blocked/done, a ring for idle, and
 * a small pip for unknown, so state reads without colour alone.
 */
export function StatusDot(props: { state: string; size?: number }): JSX.Element {
  const size = (): number => (props.state === "unknown" ? 4 : (props.size ?? 8));
  const color = (): string => agentColors[props.state] ?? agentColors.unknown ?? "#7f849c";
  const ring = (): boolean => props.state === "idle";
  return (
    <div
      aria-label={`agent ${props.state}`}
      style={{
        width: size(),
        height: size(),
        borderRadius: size(),
        flexShrink: 0,
        backgroundColor: ring() ? "transparent" : color(),
        borderWidth: ring() ? 1.5 : 0,
        borderColor: color(),
      }}
    />
  );
}

export function Pill(props: { children: string; color: string; subtle?: boolean }): JSX.Element {
  return (
    <div
      style={{
        height: 18,
        paddingLeft: 7,
        paddingRight: 7,
        borderRadius: 9,
        display: "flex",
        alignItems: "center",
        flexShrink: 0,
        backgroundColor: `${props.color}26`,
        borderWidth: props.subtle ? 0 : 1,
        borderColor: `${props.color}55`,
      }}
    >
      <text
        style={{
          color: props.color,
          fontSize: uiFont.small,
          fontWeight: 600,
          whiteSpace: "nowrap",
        }}
      >
        {props.children}
      </text>
    </div>
  );
}

export function Divider(props: { vertical?: boolean }): JSX.Element {
  const colors = usePalette();
  return (
    <div
      style={
        props.vertical
          ? { width: 1, alignSelf: "stretch", backgroundColor: colors().border, flexShrink: 0 }
          : { height: 1, alignSelf: "stretch", backgroundColor: colors().border, flexShrink: 0 }
      }
    />
  );
}

export function row(extra: StyleDesc = {}): StyleDesc {
  return { display: "flex", flexDirection: "row", alignItems: "center", ...extra };
}

export function column(extra: StyleDesc = {}): StyleDesc {
  return { display: "flex", flexDirection: "column", ...extra };
}
