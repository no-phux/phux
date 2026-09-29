/**
 * Design tokens. A theme names its terminal colours and a handful of chrome
 * surfaces; everything else (washes, borders, text tiers) is derived so a new
 * theme is eight hex values, not forty.
 */
export interface Theme {
  id: string;
  name: string;
  appearance: "dark" | "light";
  /** Terminal default background and the window base. */
  background: string;
  foreground: string;
  /** Sidebar, tab strip and panel surface. */
  surface: string;
  accent: string;
  cursor: string;
  selection: string;
  danger: string;
  warning: string;
  success: string;
}

export interface Palette extends Theme {
  raised: string;
  border: string;
  hover: string;
  active: string;
  subtext: string;
  muted: string;
  faint: string;
  accentWash: string;
  accentText: string;
  backdrop: string;
}

export const themes: readonly Theme[] = [
  {
    id: "midnight",
    name: "phux Midnight",
    appearance: "dark",
    background: "#0d1017",
    foreground: "#d8dee9",
    surface: "#12161f",
    accent: "#7aa2f7",
    cursor: "#c0caf5",
    selection: "#28406a",
    danger: "#f7768e",
    warning: "#e0af68",
    success: "#9ece6a",
  },
  {
    id: "nord",
    name: "Nord",
    appearance: "dark",
    background: "#2e3440",
    foreground: "#d8dee9",
    surface: "#292e39",
    accent: "#88c0d0",
    cursor: "#eceff4",
    selection: "#434c5e",
    danger: "#bf616a",
    warning: "#ebcb8b",
    success: "#a3be8c",
  },
  {
    id: "dracula",
    name: "Dracula",
    appearance: "dark",
    background: "#282a36",
    foreground: "#f8f8f2",
    surface: "#21222c",
    accent: "#bd93f9",
    cursor: "#f8f8f2",
    selection: "#44475a",
    danger: "#ff5555",
    warning: "#f1fa8c",
    success: "#50fa7b",
  },
  {
    id: "mocha",
    name: "Catppuccin Mocha",
    appearance: "dark",
    background: "#1e1e2e",
    foreground: "#cdd6f4",
    surface: "#181825",
    accent: "#cba6f7",
    cursor: "#f5e0dc",
    selection: "#45475a",
    danger: "#f38ba8",
    warning: "#f9e2af",
    success: "#a6e3a1",
  },
  {
    id: "latte",
    name: "Catppuccin Latte",
    appearance: "light",
    background: "#eff1f5",
    foreground: "#4c4f69",
    surface: "#e6e9ef",
    accent: "#8839ef",
    cursor: "#dc8a78",
    selection: "#ccd0da",
    danger: "#d20f39",
    warning: "#df8e1d",
    success: "#40a02b",
  },
];

export const defaultThemeId = "midnight";

export function themeById(id: string): Theme {
  return themes.find((theme) => theme.id === id) ?? fallbackTheme();
}

function fallbackTheme(): Theme {
  const first = themes[0];
  if (!first) throw new Error("No built-in themes");
  return first;
}

/** Expand a theme into every token the chrome paints with. */
export function palette(theme: Theme): Palette {
  const base = theme.background;
  const ink = theme.foreground;
  return {
    ...theme,
    raised: mix(theme.surface, ink, 0.06),
    border: mix(base, ink, 0.1),
    hover: mix(theme.surface, ink, 0.07),
    active: mix(theme.surface, ink, 0.12),
    subtext: mix(base, ink, 0.78),
    muted: mix(base, ink, 0.5),
    faint: mix(base, ink, 0.3),
    accentWash: mix(theme.surface, theme.accent, 0.2),
    accentText: readableOn(theme.accent, base, ink),
    backdrop: `${base}b8`,
  };
}

/** Agent lifecycle colours stay fixed across themes so status reads the same everywhere. */
export const agentColors: Record<string, string> = {
  working: "#f9e2af",
  blocked: "#f38ba8",
  done: "#94e2d5",
  idle: "#a6e3a1",
  unknown: "#7f849c",
};

export function mix(from: string, to: string, amount: number): string {
  const a = rgb(from);
  const b = rgb(to);
  const channel = (index: 0 | 1 | 2): string =>
    Math.round(a[index] + (b[index] - a[index]) * amount)
      .toString(16)
      .padStart(2, "0");
  return `#${channel(0)}${channel(1)}${channel(2)}`;
}

/** Whichever of two candidates contrasts more with `fill`. */
export function readableOn(fill: string, dark: string, light: string): string {
  const target = luminance(fill);
  const darkGap = Math.abs(target - luminance(dark));
  const lightGap = Math.abs(target - luminance(light));
  return darkGap > lightGap ? dark : light;
}

function luminance(hex: string): number {
  const [r, g, b] = rgb(hex).map((value) => {
    const unit = value / 255;
    return unit <= 0.039_28 ? unit / 12.92 : ((unit + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * (r ?? 0) + 0.7152 * (g ?? 0) + 0.0722 * (b ?? 0);
}

function rgb(hex: string): [number, number, number] {
  const value = Number.parseInt(hex.slice(1, 7), 16);
  if (!/^#[0-9a-f]{6}/i.test(hex) || Number.isNaN(value)) return [0, 0, 0];
  return [(value >> 16) & 0xff, (value >> 8) & 0xff, value & 0xff];
}

export const radius = { panel: 12, control: 8, small: 5 } as const;
export const uiFont = { size: 12.5, small: 11, title: 13 } as const;
