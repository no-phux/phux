/**
 * The reference theme -> roles solver (phux design spec §2.2-2.5), solver "phux-derive/2".
 *
 * No dependencies. Every port (Rust phux-design, Swift ThemePalette, Kotlin AppPalette)
 * reproduces this file bit for bit against the golden vectors (vectors.ts): IEEE-754 binary64,
 * evaluated exactly as written, luminance through the shipped LUT, never through pow.
 */

export const SOLVER = "phux-derive/2";

export type Rgb = readonly [number, number, number];
export type Rgba = readonly [number, number, number, number];
export type Mode = "dark" | "light";

export const WHITE: Rgb = [255, 255, 255];
export const BLACK: Rgb = [0, 0, 0];

export const LAYERS = ["ground", "sidebar", "raised", "overlay"] as const;
export const WASHES = ["hover", "press", "current", "selected"] as const;
export const TONES = ["needs_you", "working", "done", "failed", "idle", "unknown", "bell"] as const;
/** Tones read from an ANSI slot; idle and unknown come from the text tiers. */
export const SLOT_TONES = ["needs_you", "working", "done", "failed", "bell"] as const;
export const COMPANIONS = ["wash", "edge", "glow"] as const;
export type Layer = (typeof LAYERS)[number];
export type Tone = (typeof TONES)[number];

export interface Theme {
  bg: string;
  fg: string;
  ansi: string[];
  cursor?: string | null;
  accent?: string | null;
  selection_bg?: string | null;
  dark?: boolean | null;
}

export interface Bars {
  text: number;
  tier_ratio: number;
  tier_min: number;
  text_floor: number;
  signal: number;
  on_accent: number;
  focus: number;
  control: number;
  accent_indicator: number;
  border_strong: number;
  layer_step: number;
  pulse_trough: number;
  cursor: number;
  /** Floor for faint, unknown, selection, match and diff text. Absent means text_floor; only contrast_more sets it. */
  base_floor?: number;
}

export interface Params {
  light_threshold: number;
  bars: Bars;
  contrast_more: { text_floor: number; control: number; border_strong: number; wash_scale: number };
  iterations: number;
  slots: {
    failed: number;
    done: number;
    needs_you: number;
    working: number;
    bell: number;
    accent: number;
    match: number;
  };
  selection_mix: number;
  status_alpha: { wash: number; edge: number; glow: number };
  match_alpha: { bg: number };
  diff_alpha: number;
  max_shadow_layers: number;
  tier_surfaces: string[];
  floor_surfaces: string;
  floor_washes: string[];
  srgb_lut: string[];
  layers: Record<Mode, { sidebar: number; raised: number; overlay: number }>;
  alpha: Record<
    Mode,
    {
      border: { muted: number; base: number; strong: number };
      wash: { hover: number; press: number; current: number; selected: number };
    }
  >;
  wash_selected_source: "fg" | "accent";
  scrim: Record<Mode, number>;
  glass: { tint_min: Record<Mode, number> };
}

/** Every colour role of spec §1.2, as `#RRGGBB` (opaque) or `#RRGGBBAA` (translucent). */
export interface Roles {
  mode: Mode;
  layer: { ground: string; sidebar: string; raised: string; overlay: string; scrim: string };
  text: { text: string; dim: string; faint: string; inverse: string };
  border: { muted: string; base: string; strong: string; control: string; focus: string };
  wash: { hover: string; press: string; current: string; selected: string };
  accent: { accent: string; on_accent: string; accent_text: string; hi?: string; lo?: string };
  status: Record<string, string>;
  match: { bg: string; current_bg: string; current_fg: string };
  diff: { add: string; remove: string; add_text: string; remove_text: string };
  syntax: string[];
  selection: { bg: string; fg: string };
  flat: Record<Layer, Record<string, Record<string, string>>>;
}

export interface Violation {
  id: string;
  severity: "error" | "warning";
  role: string;
  surface: string;
  value: number;
  bar: number;
}

// --- §2.2 primitives --------------------------------------------------------------

const lutCache = new Map<string[], Float64Array>();
/** Decode the shipped IEEE-754 bit patterns. Never recompute the LUT with pow. */
export function decodeLut(bits: string[]): Float64Array {
  let lut = lutCache.get(bits);
  if (lut) return lut;
  if (bits.length !== 256) throw Error("srgb_lut must hold 256 bit patterns");
  lut = new Float64Array(256);
  const view = new DataView(new ArrayBuffer(8));
  bits.forEach((bit, i) => {
    if (!/^[0-9A-F]{16}$/.test(bit))
      throw Error(`srgb_lut[${i}] is not a 16-hex-digit bit pattern`);
    view.setBigUint64(0, BigInt(`0x${bit}`));
    lut![i] = view.getFloat64(0);
  });
  lutCache.set(bits, lut);
  return lut;
}

/** Parse `#RRGGBB`, `#RRGGBBAA`, the looks' `#rrggbb/0.07`, or `rgba(r,g,b,a)`. */
export function parse(value: string): Rgba {
  const s = value.trim();
  const rgba = /^rgba\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*([0-9.]+)\s*\)$/.exec(s);
  if (rgba)
    return [Number(rgba[1]), Number(rgba[2]), Number(rgba[3]), Math.round(Number(rgba[4]) * 255)];
  const slash = /^#([0-9a-fA-F]{6})\/([0-9.]+)$/.exec(s);
  if (slash)
    return [
      ...parse(`#${slash[1]}`).slice(0, 3),
      Math.round(Number(slash[2]) * 255),
    ] as unknown as Rgba;
  const m = /^#([0-9a-fA-F]{6})([0-9a-fA-F]{2})?$/.exec(s);
  if (!m) throw Error(`not a colour: ${value}`);
  const n = parseInt(m[1]!, 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255, m[2] ? parseInt(m[2], 16) : 255];
}

export function rgb(value: string): Rgb {
  const [r, g, b] = parse(value);
  return [r, g, b];
}

const byte = (v: number) => v.toString(16).toUpperCase().padStart(2, "0");
/** `#RRGGBB` when opaque, `#RRGGBBAA` otherwise. */
export function hex(c: Rgb | Rgba): string {
  const head = `#${byte(c[0])}${byte(c[1])}${byte(c[2])}`;
  return c.length === 3 || c[3] === 255 ? head : head + byte(c[3]!);
}

/** Round half away from zero; every caller passes x >= 0, where Math.round agrees. */
export const round = (x: number): number => Math.round(x);

export function lum(c: Rgb, lut: Float64Array): number {
  return 0.2126 * lut[c[0]]! + 0.7152 * lut[c[1]]! + 0.0722 * lut[c[2]]!;
}

export function contrast(a: Rgb, b: Rgb, lut: Float64Array): number {
  const la = lum(a, lut),
    lb = lum(b, lut);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

export function mix(a: Rgb, b: Rgb, t: number): Rgb {
  return [
    round(a[0] + (b[0] - a[0]) * t),
    round(a[1] + (b[1] - a[1]) * t),
    round(a[2] + (b[2] - a[2]) * t),
  ];
}

export function over(top: Rgba, bottom: Rgb): Rgb {
  const a = top[3] / 255;
  return [
    round(bottom[0] + (top[0] - bottom[0]) * a),
    round(bottom[1] + (top[1] - bottom[1]) * a),
    round(bottom[2] + (top[2] - bottom[2]) * a),
  ];
}

export function withAlpha(c: Rgb, alpha: number): Rgba {
  return [c[0], c[1], c[2], round(alpha * 255)];
}

export function worst(c: Rgb, surfaces: Rgb[], lut: Float64Array): number {
  let m = Infinity;
  for (const s of surfaces) m = Math.min(m, contrast(c, s, lut));
  return m;
}

export function reach(
  c: Rgb,
  toward: Rgb,
  bar: number,
  surfaces: Rgb[],
  lut: Float64Array,
  n = 24,
): Rgb {
  if (worst(c, surfaces, lut) >= bar) return c;
  let lo = 0,
    hi = 1;
  for (let i = 0; i < n; i++) {
    const mid = (lo + hi) / 2;
    if (worst(mix(c, toward, mid), surfaces, lut) >= bar) hi = mid;
    else lo = mid;
  }
  return mix(c, toward, hi);
}

/**
 * `reach` for a status tone: each candidate must clear the bar on F⁺ and on its *own* wash
 * (`candidate @ alpha` over each layer), so the exported `_wash` companion is the one tested (V13).
 */
export function reachTone(
  c: Rgb,
  toward: Rgb,
  bar: number,
  floor: Rgb[],
  layers: Rgb[],
  alpha: number,
  lut: Float64Array,
  n = 24,
): Rgb {
  const ok = (x: Rgb) => {
    if (worst(x, floor, lut) < bar) return false;
    const wash = withAlpha(x, alpha);
    for (const l of layers) if (contrast(x, over(wash, l), lut) < bar) return false;
    return true;
  };
  if (ok(c)) return c;
  let lo = 0,
    hi = 1;
  for (let i = 0; i < n; i++) {
    const mid = (lo + hi) / 2;
    if (ok(mix(c, toward, mid))) hi = mid;
    else lo = mid;
  }
  return mix(c, toward, hi);
}

export function fade(
  c: Rgb,
  toward: Rgb,
  target: number,
  surfaces: Rgb[],
  lut: Float64Array,
  n = 24,
): Rgb {
  let lo = 0,
    hi = 1;
  for (let i = 0; i < n; i++) {
    const mid = (lo + hi) / 2;
    if (worst(mix(c, toward, mid), surfaces, lut) >= target) lo = mid;
    else hi = mid;
  }
  return mix(c, toward, lo);
}

export function onFill(f: Rgb, lut: Float64Array): Rgb {
  return contrast(WHITE, f, lut) >= contrast(BLACK, f, lut) ? WHITE : BLACK;
}

// --- surfaces -----------------------------------------------------------------------

export interface Surface {
  name: string;
  color: Rgb;
}

export function tierSurfaces(layer: Record<Layer, Rgb>): Surface[] {
  return LAYERS.map((name) => ({ name, color: layer[name] }));
}

/** F⁺: T, then every wash over every layer, wash-major (`raised+selected`). */
export function floorSurfaces(layer: Record<Layer, Rgb>, wash: Record<string, Rgba>): Surface[] {
  const out = tierSurfaces(layer);
  for (const w of WASHES)
    for (const l of LAYERS) out.push({ name: `${l}+${w}`, color: over(wash[w]!, layer[l]) });
  return out;
}

const colors = (s: Surface[]) => s.map((x) => x.color);

// --- §2.3 derive --------------------------------------------------------------------

export function modeOf(theme: Theme, P: Params): Mode {
  if (theme.dark === true) return "dark";
  if (theme.dark === false) return "light";
  return lum(rgb(theme.bg), decodeLut(P.srgb_lut)) > P.light_threshold ? "light" : "dark";
}

/** The base floor (faint, unknown, selection, match, diff); text_floor unless contrast_more set it. */
export const baseFloor = (b: Bars) => b.base_floor ?? b.text_floor;

export function derive(theme: Theme, P: Params): Roles {
  const lut = decodeLut(P.srgb_lut),
    n = P.iterations,
    B = P.bars;
  const bg = rgb(theme.bg),
    fg = rgb(theme.fg),
    ansi = theme.ansi.map(rgb);
  if (ansi.length !== 16) throw Error("a theme has 16 ANSI colours");
  const mode = modeOf(theme, P);
  const extreme = mode === "light" ? BLACK : WHITE;
  const away = mode === "light" ? WHITE : BLACK;
  const layer = { ground: bg } as Record<Layer, Rgb>;
  for (const k of ["sidebar", "raised", "overlay"] as const) {
    const s = P.layers[mode][k];
    layer[k] = s >= 0 ? mix(bg, fg, s) : mix(bg, away, -s);
  }
  const A = P.alpha[mode];
  const accentRaw = theme.accent ? rgb(theme.accent) : ansi[P.slots.accent]!;
  const border = {
    muted: withAlpha(fg, A.border.muted),
    base: withAlpha(fg, A.border.base),
    strong: withAlpha(fg, A.border.strong),
  };
  const wash: Record<string, Rgba> = {
    hover: withAlpha(fg, A.wash.hover),
    press: withAlpha(fg, A.wash.press),
    current: withAlpha(fg, A.wash.current),
    selected: withAlpha(P.wash_selected_source === "accent" ? accentRaw : fg, A.wash.selected),
  };
  const T = colors(tierSurfaces(layer));
  const F = colors(floorSurfaces(layer, wash));

  let text = reach(fg, extreme, B.text, T, lut, n);
  let w = T[0]!;
  for (const s of T) if (contrast(text, s, lut) < contrast(text, w, lut)) w = s;
  const p = contrast(text, w, lut);
  let dim = fade(text, w, p / B.tier_ratio, T, lut, n);
  let faint = fade(text, w, p / (B.tier_ratio * B.tier_ratio), T, lut, n);
  const tone: Record<string, Rgb> = {};
  for (const t of SLOT_TONES) tone[t] = reach(ansi[P.slots[t]]!, extreme, B.signal, T, lut, n);
  tone.idle = mix(dim, faint, 0.5);
  let accentText = reach(accentRaw, extreme, B.signal, T, lut, n);

  // Floor pass: words read on every wash and on their own tone wash.
  text = reach(text, extreme, B.text_floor, F, lut, n);
  dim = reach(dim, extreme, B.text_floor, F, lut, n);
  accentText = reach(accentText, extreme, B.text_floor, F, lut, n);
  for (const t of [...SLOT_TONES, "idle"])
    tone[t] = reachTone(tone[t]!, extreme, B.text_floor, F, T, P.status_alpha.wash, lut, n);
  faint = reach(faint, extreme, baseFloor(B), T, lut, n);
  tone.unknown = faint;

  const onAccent = onFill(accentRaw, lut);
  const focus = reach(accentRaw, extreme, B.focus, F, lut, n);
  const control = reach(over(border.strong, layer.raised), extreme, B.control, T, lut, n);
  const selBg = theme.selection_bg ? rgb(theme.selection_bg) : mix(bg, accentRaw, P.selection_mix);
  const selFg = reach(text, onFill(selBg, lut), baseFloor(B), [selBg], lut, n);

  return complete(
    {
      mode,
      layer: { ...hexes(layer), scrim: hex(withAlpha(BLACK, P.scrim[mode])) } as Roles["layer"],
      text: { text: hex(text), dim: hex(dim), faint: hex(faint), inverse: hex(bg) },
      border: {
        muted: hex(border.muted),
        base: hex(border.base),
        strong: hex(border.strong),
        control: hex(control),
        focus: hex(focus),
      },
      wash: hexes(wash) as Roles["wash"],
      accent: { accent: hex(accentRaw), on_accent: hex(onAccent), accent_text: hex(accentText) },
      status: Object.fromEntries(TONES.map((t) => [t, hex(tone[t]!)])),
      selection: { bg: hex(selBg), fg: hex(selFg) },
    },
    theme.ansi,
    P,
  );
}

function hexes(o: Record<string, Rgb | Rgba>): Record<string, string> {
  return Object.fromEntries(Object.entries(o).map(([k, v]) => [k, hex(v)]));
}

export type CoreRoles = Omit<Roles, "match" | "diff" | "syntax" | "flat">;

/**
 * Fill the computed roles from the core ones: status companions, match, diff and syntax
 * (from the theme's ANSI colours), and the flat forms. Shared by derive and by authored looks.
 */
export function complete(core: CoreRoles, ansiHex: string[], P: Params): Roles {
  const lut = decodeLut(P.srgb_lut),
    n = P.iterations,
    B = P.bars,
    floor = baseFloor(B);
  const extreme = core.mode === "light" ? BLACK : WHITE;
  const ansi = ansiHex.map(rgb);
  const raised = rgb(core.layer.raised);
  const status: Record<string, string> = {};
  for (const t of TONES) {
    const c = rgb(core.status[t]!);
    status[t] = hex(c);
    for (const k of COMPANIONS) status[`${t}_${k}`] = hex(withAlpha(c, P.status_alpha[k]));
  }
  const m = ansi[P.slots.match]!;
  const add = withAlpha(ansi[2]!, P.diff_alpha),
    remove = withAlpha(ansi[1]!, P.diff_alpha);
  const roles: Roles = {
    ...core,
    status,
    match: {
      bg: hex(withAlpha(m, P.match_alpha.bg)),
      current_bg: hex(m),
      current_fg: hex(onFill(m, lut)),
    },
    diff: {
      add: hex(add),
      remove: hex(remove),
      add_text: hex(reach(ansi[2]!, extreme, floor, [over(add, raised)], lut, n)),
      remove_text: hex(reach(ansi[1]!, extreme, floor, [over(remove, raised)], lut, n)),
    },
    syntax: ansi.map((c) => hex(reach(c, extreme, floor, [raised], lut, n))),
    flat: {} as Roles["flat"],
  };
  roles.flat = flatten(roles);
  return roles;
}

/**
 * The translucent roles of §1.2. Their flat forms always exist, so every consumer sees one
 * shape; a variant that makes one opaque (contrast_more's border.strong) flattens to itself.
 */
export const TRANSLUCENT: Record<string, readonly string[]> = {
  layer: ["scrim"],
  border: ["muted", "base", "strong"],
  wash: WASHES,
  status: TONES.flatMap((t) => COMPANIONS.map((k) => `${t}_${k}`)),
  match: ["bg"],
  diff: ["add", "remove"],
};

/** Every translucent role composited over each layer of T (8-bit `over`). */
export function flatten(r: Roles): Roles["flat"] {
  const out = {} as Roles["flat"];
  for (const l of LAYERS) {
    const base = rgb(r.layer[l]);
    out[l] = Object.fromEntries(
      Object.entries(TRANSLUCENT).map(([group, names]) => [
        group,
        Object.fromEntries(
          names.map((name) => [name, hex(over(parse((r as any)[group][name]), base))]),
        ),
      ]),
    );
  }
  return out;
}
