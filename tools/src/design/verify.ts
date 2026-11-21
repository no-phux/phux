/** The verify checks (spec §2.5): V1-V9, V11-V13 and V14-V17 for the house look. */
import {
  BLACK,
  LAYERS,
  TONES,
  WASHES,
  WHITE,
  baseFloor,
  contrast,
  decodeLut,
  floorSurfaces,
  over,
  parse,
  rgb,
  tierSurfaces,
  withAlpha,
  type Layer,
  type Params,
  type Rgb,
  type Rgba,
  type Roles,
  type Surface,
  type Violation,
} from "./derive.ts";

export interface HouseExtras {
  /** material.glass.tint[mode] as a layer colour plus the authored and minimum alpha. */
  glass?: { tint: string; alpha: number; tint_min: number };
  /** motion.loop.pulse.min_opacity. */
  pulse_min_opacity?: number;
  /** elevation[mode] tiers. */
  elevation?: Record<string, { kind?: string }[]>;
}

export interface VerifyOptions {
  context: "house" | "derived";
  /** The theme's (or house terminal's) cursor and background, for V17. */
  cursor?: string | null;
  bg?: string;
  house?: HouseExtras;
  /** Checks reported as warnings in this context (contrast_more lets the tiers merge: V3). */
  warn?: string[];
}

const HOUSE_ERROR_DERIVED_WARNING = new Set(["V3", "V6", "V8", "V11", "V17"]);

export function verify(
  r: Roles,
  P: Params,
  opts: VerifyOptions = { context: "derived" },
): Violation[] {
  const lut = decodeLut(P.srgb_lut),
    B = P.bars,
    out: Violation[] = [];
  const severity = (id: string): Violation["severity"] =>
    (HOUSE_ERROR_DERIVED_WARNING.has(id) && opts.context === "derived") || opts.warn?.includes(id)
      ? "warning"
      : "error";
  const layer = Object.fromEntries(LAYERS.map((l) => [l, rgb(r.layer[l])])) as Record<Layer, Rgb>;
  const wash = Object.fromEntries(WASHES.map((w) => [w, parse(r.wash[w])])) as Record<string, Rgba>;
  const T = tierSurfaces(layer),
    F = floorSurfaces(layer, wash);
  const floor = baseFloor(B);

  /** Report the worst surface below the bar, once per role. */
  function need(id: string, role: string, color: string, bar: number, surfaces: Surface[]): void {
    const c = rgb(color);
    let at = surfaces[0]!,
      m = Infinity;
    for (const s of surfaces) {
      const v = contrast(c, s.color, lut);
      if (v < m) {
        m = v;
        at = s;
      }
    }
    if (m < bar - 1e-9)
      out.push({ id, severity: severity(id), role, surface: at.name, value: m, bar });
  }
  const tone = (t: string) => r.status[t]!;

  for (const [role, c] of [
    ["text.text", r.text.text],
    ["text.dim", r.text.dim],
    ["accent.accent_text", r.accent.accent_text],
  ] as const)
    need("V1", role, c, B.text_floor, F);
  for (const t of TONES) if (t !== "unknown") need("V1", `status.${t}`, tone(t), B.text_floor, F);
  need("V2", "text.faint", r.text.faint, floor, T);
  need("V2", "status.unknown", tone("unknown"), floor, T);

  const text = rgb(r.text.text),
    dim = rgb(r.text.dim),
    faint = rgb(r.text.faint);
  let w = T[0]!;
  for (const s of T) if (contrast(text, s.color, lut) < contrast(text, w.color, lut)) w = s;
  const ct = contrast(text, w.color, lut),
    cd = contrast(dim, w.color, lut),
    cf = contrast(faint, w.color, lut);
  if (ct / cd < B.tier_min - 1e-9)
    out.push({
      id: "V3",
      severity: severity("V3"),
      role: "text.dim",
      surface: w.name,
      value: ct / cd,
      bar: B.tier_min,
    });
  if (cd / cf < B.tier_min - 1e-9)
    out.push({
      id: "V3",
      severity: severity("V3"),
      role: "text.faint",
      surface: w.name,
      value: cd / cf,
      bar: B.tier_min,
    });

  need("V4", "accent.on_accent", r.accent.on_accent, B.on_accent, [
    { name: "accent.accent", color: rgb(r.accent.accent) },
  ]);
  need("V4", "text.inverse", r.text.inverse, B.on_accent, [{ name: "text.text", color: text }]);
  need("V5", "border.focus", r.border.focus, B.focus, F);

  const strong = parse(r.border.strong);
  let sm = Infinity,
    sAt = T[0]!;
  for (const s of T) {
    const v = contrast(over(strong, s.color), s.color, lut);
    if (v < sm) {
      sm = v;
      sAt = s;
    }
  }
  if (sm < B.border_strong - 1e-9)
    out.push({
      id: "V6",
      severity: severity("V6"),
      role: "border.strong",
      surface: sAt.name,
      value: sm,
      bar: B.border_strong,
    });

  need("V7", "selection.fg", r.selection.fg, floor, [
    { name: "selection.bg", color: rgb(r.selection.bg) },
  ]);
  const step = contrast(layer.ground, layer.raised, lut);
  if (step < B.layer_step - 1e-9)
    out.push({
      id: "V8",
      severity: severity("V8"),
      role: "layer.raised",
      surface: "ground",
      value: step,
      bar: B.layer_step,
    });
  if (wash.press![3] <= wash.hover![3])
    out.push({
      id: "V9",
      severity: "error",
      role: "wash.press",
      surface: "wash.hover",
      value: wash.press![3] / 255,
      bar: wash.hover![3] / 255,
    });

  need("V11", "accent.accent", r.accent.accent, B.accent_indicator, T);
  need("V12", "border.control", r.border.control, B.control, T);
  for (const t of TONES) {
    if (t === "unknown") continue;
    need(
      "V13",
      `status.${t}`,
      tone(t),
      B.text_floor,
      LAYERS.map((l) => ({
        name: `${l}+${t}_wash`,
        color: over(parse(r.status[`${t}_wash`]!), layer[l]),
      })),
    );
  }
  need("V13", "diff.add_text", r.diff.add_text, floor, [
    { name: "raised+diff.add", color: over(parse(r.diff.add), layer.raised) },
  ]);
  need("V13", "diff.remove_text", r.diff.remove_text, floor, [
    { name: "raised+diff.remove", color: over(parse(r.diff.remove), layer.raised) },
  ]);
  need("V13", "match.current_fg", r.match.current_fg, floor, [
    { name: "match.current_bg", color: rgb(r.match.current_bg) },
  ]);

  const h = opts.house;
  if (h?.glass) {
    const tint = rgb(h.glass.tint);
    for (const a of [h.glass.tint_min, h.glass.alpha]) {
      const glass = withAlpha(tint, a);
      const backdrops = [
        { name: `glass@${a}+black`, color: over(glass, BLACK) },
        { name: `glass@${a}+white`, color: over(glass, WHITE) },
      ];
      need("V14", "text.text", r.text.text, B.text_floor, backdrops);
      need("V14", "text.dim", r.text.dim, B.text_floor, backdrops);
    }
    if (h.glass.alpha < h.glass.tint_min - 1e-9)
      out.push({
        id: "V14",
        severity: "error",
        role: "material.glass.tint",
        surface: "tint_min",
        value: h.glass.alpha,
        bar: h.glass.tint_min,
      });
  }
  if (h?.pulse_min_opacity !== undefined) {
    const mark = rgb(tone("working"));
    let m = Infinity,
      at = T[0]!;
    for (const s of T) {
      const v = contrast(over(withAlpha(mark, h.pulse_min_opacity), s.color), s.color, lut);
      if (v < m) {
        m = v;
        at = s;
      }
    }
    if (m < B.pulse_trough - 1e-9)
      out.push({
        id: "V15",
        severity: "error",
        role: "status.working",
        surface: at.name,
        value: m,
        bar: B.pulse_trough,
      });
  }
  if (h?.elevation) {
    for (const [tier, layers] of Object.entries(h.elevation)) {
      const count = (k: string) => layers.filter((x) => x.kind === k).length;
      const bad =
        layers.length > P.max_shadow_layers ||
        count("ring") > 1 ||
        count("lit") > 1 ||
        count("glow") > 1;
      if (bad)
        out.push({
          id: "V16",
          severity: "error",
          role: `elevation.${tier}`,
          surface: tier,
          value: layers.length,
          bar: P.max_shadow_layers,
        });
    }
  }
  if (opts.cursor && opts.bg)
    need("V17", "terminal.cursor", opts.cursor, B.cursor, [
      { name: "terminal.bg", color: rgb(opts.bg) },
    ]);
  return out;
}
