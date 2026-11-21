/** design-gen --suggest: the least move that clears each failing authored floor. */
import {
  BLACK,
  LAYERS,
  SLOT_TONES,
  WASHES,
  WHITE,
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
  withAlpha,
  type CoreRoles,
  type Layer,
  type Mode,
  type Params,
  type Rgb,
  type Roles,
} from "./derive.ts";
import { fitParams } from "./fit.ts";
import { MODES, coreOf, validateHouseColor, type Sources } from "./sources.ts";
import { spaceTiers } from "./variants.ts";

/** The least move that clears each authored floor, for house and reader (spec §2.6). */
export function suggest(S: Sources): Record<string, Record<string, { from: string; to: string }>> {
  for (const mode of MODES) validateHouseColor(mode, S.house.color[mode]);
  const base = { ...S.deriveConstants } as Params;
  const houseRoles = Object.fromEntries(
    MODES.map((m) => [m, complete(coreOf(m, S.house.color[m]), S.house.terminal[m].ansi, base)]),
  ) as Record<Mode, Roles>;
  const P = fitParams(S, houseRoles),
    lut = decodeLut(P.srgb_lut),
    B = P.bars,
    n = P.iterations;
  const out: Record<string, Record<string, { from: string; to: string }>> = {};
  const looks: [string, Mode, any][] = MODES.flatMap(
    (m) =>
      [
        ["house", m, S.house.color[m]],
        ["reader", m, S.reader.color[m]],
      ] as [string, Mode, any][],
  );
  for (const [look, mode, c] of looks) {
    const fixes: Record<string, { from: string; to: string }> = {};
    const extreme = mode === "light" ? BLACK : WHITE;
    const layer = Object.fromEntries(LAYERS.map((l) => [l, rgb(c.layer[l])])) as Record<Layer, Rgb>;
    const wash = Object.fromEntries(WASHES.map((w) => [w, parse(c.wash[w])]));
    const T = tierSurfaces(layer).map((s) => s.color),
      F = floorSurfaces(layer, wash).map((s) => s.color);
    const move = (path: string, from: string, to: string) => {
      if (from !== to) fixes[path] = { from, to };
    };
    const fix = (path: string, bar: number, surfaces: Rgb[], toward: Rgb = extreme) => {
      const [g, k] = path.split(".") as [string, string];
      if (!c[g]?.[k]) return;
      const to = hex(reach(rgb(c[g][k]), toward, bar, surfaces, lut, n));
      move(path, c[g][k], to);
      c[g][k] = to;
    };
    for (const p of ["text.text", "text.dim", "accent.accent_text"]) fix(p, B.text_floor, F);
    fix("text.faint", B.text_floor, T);
    if (look === "house") {
      for (const t of [...SLOT_TONES, "idle"]) {
        const to = hex(
          reachTone(rgb(c.status[t]), extreme, B.text_floor, F, T, P.status_alpha.wash, lut, n),
        );
        move(`status.${t}`, c.status[t], to);
        c.status[t] = to;
      }
      fix("status.unknown", B.text_floor, T);
      const control = hex(
        reach(over(parse(c.border.strong), layer.raised), extreme, B.control, T, lut, n),
      );
      if (contrastWorst(c.border.control, T, lut) < B.control)
        move("border.control", c.border.control, control);
    }
    fix("border.focus", B.focus, F);
    fix("selection.fg", B.text_floor, [rgb(c.selection.bg)], onFill(rgb(c.selection.bg), lut));
    const core = { mode, text: { ...c.text }, layer: c.layer } as unknown as CoreRoles;
    spaceTiers(core, P);
    move("text.dim", c.text.dim, core.text.dim);
    out[`${look}/${mode}`] = fixes;
  }
  const g = S.house.material.glass.tint;
  for (const mode of MODES) {
    const min = P.glass.tint_min[mode];
    if (g[mode].alpha < min)
      (out[`house/${mode}`] ??= {})["material.glass.tint.alpha"] = {
        from: String(g[mode].alpha),
        to: String(min),
      };
  }
  const pulse = S.house.motion.loop.pulse.min_opacity;
  let least = pulse;
  for (const mode of MODES) {
    const r = houseRoles[mode],
      mark = rgb(r.status.working!);
    const T = LAYERS.map((l) => rgb(r.layer[l]));
    for (let a = least; a <= 1.0001; a = Math.round((a + 0.01) * 100) / 100) {
      if (T.every((s) => contrast(over(withAlpha(mark, a), s), s, lut) >= B.pulse_trough)) {
        least = a;
        break;
      }
    }
  }
  if (least !== pulse)
    (out.house_motion ??= {})["motion.loop.pulse.min_opacity"] = {
      from: String(pulse),
      to: String(least),
    };
  return out;
}

function contrastWorst(c: string, S: Rgb[], lut: Float64Array): number {
  return Math.min(...S.map((s) => contrast(rgb(c), s, lut)));
}
