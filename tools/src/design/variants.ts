/** The reader and increased-contrast variants, solved from the house roles by the same primitives. */
import {
  BLACK,
  LAYERS,
  SLOT_TONES,
  WASHES,
  WHITE,
  baseFloor,
  complete,
  contrast,
  decodeLut,
  floorSurfaces,
  hex,
  onFill,
  over,
  parse,
  reach,
  reachTone,
  rgb,
  tierSurfaces,
  type CoreRoles,
  type Layer,
  type Mode,
  type Params,
  type Rgb,
  type Roles,
} from "./derive.ts";
import type { Sources } from "./sources.ts";

/** Make dim and faint at least tier_min apart on the worst tier surface, by the least move. */
export function spaceTiers(core: CoreRoles, P: Params): void {
  const lut = decodeLut(P.srgb_lut),
    B = P.bars,
    floor = baseFloor(B);
  const layer = Object.fromEntries(LAYERS.map((l) => [l, rgb(core.layer[l])])) as Record<
    Layer,
    Rgb
  >;
  const T = tierSurfaces(layer).map((s) => s.color);
  const extreme = core.mode === "light" ? BLACK : WHITE;
  const text = rgb(core.text.text);
  let w = T[0]!;
  for (const s of T) if (contrast(text, s, lut) < contrast(text, w, lut)) w = s;
  const c = (x: string) => contrast(rgb(x), w, lut);
  if (c(core.text.dim) / c(core.text.faint) >= B.tier_min) return;
  // Push dim toward the extreme until faint (at least at its floor) sits tier_min below it.
  const target = Math.max(c(core.text.faint), floor) * B.tier_min;
  core.text.dim = hex(reach(rgb(core.text.dim), extreme, target, [w], lut, P.iterations));
}

/** The docs-reader variant: authored surfaces and ink, with the house tones solved onto them. */
export function solveReader(S: Sources, mode: Mode, house: Roles, P: Params): Roles {
  const lut = decodeLut(P.srgb_lut),
    B = P.bars,
    n = P.iterations;
  const c = S.reader.color[mode];
  const extreme = mode === "light" ? BLACK : WHITE;
  const layer = Object.fromEntries(LAYERS.map((l) => [l, rgb(c.layer[l])])) as Record<Layer, Rgb>;
  const wash = Object.fromEntries(WASHES.map((w) => [w, parse(c.wash[w])]));
  const T = tierSurfaces(layer).map((s) => s.color),
    F = floorSurfaces(layer, wash).map((s) => s.color);
  const status: Record<string, string> = {};
  for (const t of [...SLOT_TONES, "idle"]) {
    status[t] = hex(
      reachTone(rgb(house.status[t]!), extreme, B.text_floor, F, T, P.status_alpha.wash, lut, n),
    );
  }
  status.unknown = c.text.faint;
  const core: CoreRoles = {
    mode,
    layer: { ...c.layer },
    text: { ...c.text },
    border: {
      muted: c.border.muted,
      base: c.border.base,
      strong: c.border.strong,
      control: hex(
        reach(over(parse(c.border.strong), layer.raised), extreme, B.control, T, lut, n),
      ),
      focus: c.border.focus,
    },
    wash: { ...c.wash },
    accent: {
      accent: c.accent.accent,
      on_accent: c.accent.on_accent,
      accent_text: hex(reach(rgb(c.accent.accent), extreme, B.text_floor, F, lut, n)),
    },
    status,
    selection: { ...c.selection },
  };
  return complete(core, S.house.terminal[mode].ansi, P);
}

/** Params with the contrast_more bars (spec §1.11); faint, selection, match and diff keep the base floor. */
export function contrastMoreParams(P: Params): Params {
  const cm = P.contrast_more;
  return {
    ...P,
    bars: {
      ...P.bars,
      text_floor: cm.text_floor,
      control: cm.control,
      border_strong: cm.border_strong,
      base_floor: P.bars.text_floor,
    },
  };
}

/** The increased-contrast variant, solved from the house roles by the same primitives. */
export function solveContrastMore(S: Sources, mode: Mode, house: Roles, P: Params): Roles {
  const Q = contrastMoreParams(P),
    lut = decodeLut(P.srgb_lut),
    B = Q.bars,
    n = P.iterations;
  const extreme = mode === "light" ? BLACK : WHITE;
  const layer = Object.fromEntries(LAYERS.map((l) => [l, rgb(house.layer[l])])) as Record<
    Layer,
    Rgb
  >;
  const scaled = Object.fromEntries(
    WASHES.map((w) => {
      const c = parse(house.wash[w]);
      return [
        w,
        [c[0], c[1], c[2], Math.min(255, Math.round(c[3] * P.contrast_more.wash_scale))] as const,
      ];
    }),
  );
  const T = tierSurfaces(layer).map((s) => s.color),
    F = floorSurfaces(layer, scaled).map((s) => s.color);
  const up = (x: string, bar: number, surfaces: Rgb[]) =>
    hex(reach(rgb(x), extreme, bar, surfaces, lut, n));
  const status: Record<string, string> = {};
  for (const t of [...SLOT_TONES, "idle"]) {
    status[t] = hex(
      reachTone(rgb(house.status[t]!), extreme, B.text_floor, F, T, P.status_alpha.wash, lut, n),
    );
  }
  const faint = up(house.text.faint, baseFloor(B), T);
  status.unknown = faint;
  const selBg = rgb(house.selection.bg);
  const core: CoreRoles = {
    mode,
    layer: { ...house.layer },
    text: {
      text: up(house.text.text, B.text_floor, F),
      dim: up(house.text.dim, B.text_floor, F),
      faint,
      inverse: house.text.inverse,
    },
    border: {
      muted: house.border.muted,
      base: house.border.base,
      strong: hex(
        reach(over(parse(house.border.strong), layer.raised), extreme, B.border_strong, T, lut, n),
      ),
      control: up(house.border.control, B.control, T),
      focus: up(house.border.focus, B.focus, F),
    },
    wash: Object.fromEntries(WASHES.map((w) => [w, hex(scaled[w]!)])) as Roles["wash"],
    accent: {
      accent: house.accent.accent,
      on_accent: house.accent.on_accent,
      accent_text: up(house.accent.accent_text, B.text_floor, F),
    },
    status,
    selection: {
      bg: house.selection.bg,
      fg: hex(reach(rgb(house.selection.fg), onFill(selBg, lut), baseFloor(B), [selBg], lut, n)),
    },
  };
  spaceTiers(core, Q);
  return complete(core, S.house.terminal[mode].ansi, Q);
}
