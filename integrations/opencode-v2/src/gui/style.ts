/**
 * The GUI's styles. App tokens (`--v2-*`) keep the panel in the user's theme;
 * each has a fallback, since they are not a stable plugin API.
 */

const DARK_ANSI = [
  "#1d1f21", "#cc6666", "#b5bd68", "#f0c674", "#81a2be", "#b294bb", "#8abeb7", "#c5c8c6",
  "#666666", "#d54e53", "#b9ca4a", "#e7c547", "#7aa6da", "#c397d8", "#70c0b1", "#eaeaea",
];
const LIGHT_ANSI = [
  "#1d1f21", "#c82829", "#718c00", "#b58900", "#4271ae", "#8959a8", "#3e999f", "#8e908c",
  "#4d4d4c", "#e53935", "#5f8700", "#a07000", "#3465a4", "#75507b", "#06989a", "#1d1f21",
];

/** The xterm 256-colour cube and grey ramp above the 16 theme colours. */
function extendedPalette(): string {
  const levels = [0, 95, 135, 175, 215, 255];
  const declarations: string[] = [];
  for (let index = 16; index < 232; index++) {
    const value = index - 16;
    const r = levels[Math.floor(value / 36)]!;
    const g = levels[Math.floor(value / 6) % 6]!;
    const b = levels[value % 6]!;
    declarations.push(`--vt-palette-${index}: rgb(${r}, ${g}, ${b});`);
  }
  for (let index = 232; index < 256; index++) {
    const level = 8 + (index - 232) * 10;
    declarations.push(`--vt-palette-${index}: rgb(${level}, ${level}, ${level});`);
  }
  return declarations.join("");
}

const ansi = (colors: readonly string[]) => colors.map((color, index) => `--vt-palette-${index}: ${color};`).join("");

export const css = `
.phux-root {
  --phux-fg: var(--v2-text-text-base, currentColor);
  --phux-muted: var(--v2-text-text-muted, color-mix(in srgb, currentColor 65%, transparent));
  --phux-faint: var(--v2-text-text-faint, color-mix(in srgb, currentColor 45%, transparent));
  --phux-border: var(--v2-border-border-muted, color-mix(in srgb, currentColor 14%, transparent));
  --phux-hover: var(--v2-overlay-simple-overlay-hover, color-mix(in srgb, currentColor 7%, transparent));
  --phux-layer: var(--v2-background-bg-layer-01, color-mix(in srgb, currentColor 4%, transparent));
  --phux-danger: var(--v2-state-fg-danger, #e5484d);
  --phux-warning: var(--v2-state-fg-warning, #f5a524);
  --phux-success: var(--v2-state-fg-success, #30a46c);
  --phux-info: var(--v2-state-fg-info, #3e8ef7);
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  color: var(--phux-fg);
  font-family: var(--v2-font-family-sans, system-ui, sans-serif);
  font-size: 13px;
}
.phux-root.phux-dark { ${ansi(DARK_ANSI)} }
.phux-root.phux-light { ${ansi(LIGHT_ANSI)} }
.phux-root { ${extendedPalette()} }

.phux-dot {
  flex: none;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--phux-faint, currentColor);
  opacity: 0.8;
}
.phux-dot[data-state="working"] { background: var(--phux-info, #3e8ef7); opacity: 1; animation: phux-pulse 1.6s ease-in-out infinite; }
.phux-dot[data-state="blocked"] { background: var(--phux-danger, #e5484d); opacity: 1; }
.phux-dot[data-state="done"] { background: var(--phux-success, #30a46c); opacity: 1; }
.phux-dot[data-state="closed"] { background: transparent; border: 1px solid var(--phux-faint, currentColor); }
@keyframes phux-pulse { 50% { opacity: 0.35; } }
@media (prefers-reduced-motion: reduce) { .phux-dot[data-state="working"] { animation: none; } }

.phux-tab { display: inline-flex; align-items: center; gap: 6px; min-width: 0; max-width: 220px; }
.phux-tab > span:last-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

.phux-header {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 10px 8px 12px;
  border-bottom: 1px solid var(--phux-border);
  min-height: 40px;
}
.phux-header-text { display: flex; flex-direction: column; min-width: 0; flex: 1; }
.phux-header-title { font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.phux-header-meta { color: var(--phux-muted); font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.phux-header-actions { display: flex; align-items: center; gap: 2px; flex: none; }
.phux-header-reserve { width: 32px; flex: none; }

.phux-search { padding: 8px 10px; border-bottom: 1px solid var(--phux-border); }
.phux-list { flex: 1; min-height: 0; overflow: auto; padding: 4px 0 12px; }
.phux-group { padding: 10px 12px 4px; color: var(--phux-muted); font-size: 11px; font-weight: 500; letter-spacing: 0.02em; text-transform: uppercase; display: flex; gap: 6px; }
.phux-group-count { color: var(--phux-faint); }
.phux-row {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  width: 100%;
  padding: 7px 12px;
  border: 0;
  background: transparent;
  color: inherit;
  font: inherit;
  text-align: left;
  cursor: pointer;
}
.phux-row:hover, .phux-row:focus-visible { background: var(--phux-hover); outline: none; }
.phux-row .phux-dot { margin-top: 5px; }
.phux-row-text { display: flex; flex-direction: column; min-width: 0; flex: 1; gap: 1px; }
.phux-row-title { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.phux-row-meta { color: var(--phux-muted); font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.phux-row-state { flex: none; color: var(--phux-muted); font-size: 12px; margin-top: 1px; }
.phux-row-state[data-state="blocked"] { color: var(--phux-danger); }
.phux-badge {
  display: inline-block;
  margin-left: 6px;
  padding: 0 5px;
  border-radius: 4px;
  border: 1px solid var(--phux-border);
  color: var(--phux-muted);
  font-size: 11px;
  line-height: 16px;
  vertical-align: 1px;
}

.phux-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  flex: 1;
  padding: 24px;
  color: var(--phux-muted);
  text-align: center;
}
.phux-empty code { font-family: var(--v2-font-family-code, ui-monospace, monospace); font-size: 12px; }
.phux-empty-title { color: var(--phux-fg); font-weight: 500; }
.phux-error { font-family: var(--v2-font-family-code, ui-monospace, monospace); font-size: 11px; max-width: 100%; overflow-wrap: anywhere; color: var(--phux-faint); }

.phux-screen-wrap { position: relative; flex: 1; min-height: 0; display: flex; }
.phux-screen {
  flex: 1;
  min-width: 0;
  overflow: auto;
  padding: 8px 10px;
  line-height: 1.3;
  white-space: pre;
  cursor: text;
  outline: none;
  background: var(--phux-layer);
  font-variant-ligatures: none;
}
.phux-screen[data-typing="true"] { box-shadow: inset 0 0 0 1px var(--v2-border-border-focus, var(--phux-info)); }
.phux-line { min-height: 1.3em; }
.phux-caret { outline: 1px solid var(--phux-fg); outline-offset: -1px; }
.phux-screen[data-typing="true"] .phux-caret { background: var(--phux-fg); color: var(--v2-background-bg-base, #fff); outline: 0; }
.phux-jump { position: absolute; right: 14px; bottom: 10px; }
.phux-hint {
  padding: 4px 12px;
  color: var(--phux-muted);
  font-size: 12px;
  border-top: 1px solid var(--phux-border);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.phux-input { display: flex; flex-direction: column; gap: 6px; padding: 8px 10px 10px; border-top: 1px solid var(--phux-border); }
.phux-input-row { display: flex; gap: 6px; align-items: center; }
.phux-input-row > :first-child { flex: 1; min-width: 0; }
.phux-keys { display: flex; flex-wrap: wrap; gap: 4px; }
.phux-key {
  border: 1px solid var(--phux-border);
  border-radius: 5px;
  background: transparent;
  color: var(--phux-muted);
  font: 11px/18px var(--v2-font-family-code, ui-monospace, monospace);
  padding: 0 6px;
  cursor: pointer;
}
.phux-key:hover { background: var(--phux-hover); color: var(--phux-fg); }

.phux-settings-status { display: inline-flex; align-items: center; gap: 8px; }
`;
