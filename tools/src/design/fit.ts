/** The derive block fitted from the house look (spec §2.4), and the OKLab checks only the generator runs. */
import { createHash } from "node:crypto";
import {
  BLACK,
  WHITE,
  contrast,
  decodeLut,
  hex,
  mix,
  over,
  parse,
  rgb,
  type Mode,
  type Params,
  type Rgb,
  type Roles,
  type Violation,
} from "./derive.ts";
import { MODES, type Sources } from "./sources.ts";

/** Shortest decimal on a 0.001 grid that encodes the 8-bit alpha A exactly. */
export function alphaOf(A: number): number {
  const a = Math.round((A / 255) * 1000) / 1000;
  if (Math.round(a * 255) !== A) throw Error(`alpha ${A} has no 3-decimal encoding`);
  return a;
}

function stepFit(bg: Rgb, fg: Rgb, target: Rgb, away: Rgb): number {
  const cost = (t: number) => {
    const c = t >= 0 ? mix(bg, fg, t) : mix(bg, away, -t);
    return Math.abs(c[0] - target[0]) + Math.abs(c[1] - target[1]) + Math.abs(c[2] - target[2]);
  };
  let best = 0,
    bestCost = cost(0);
  for (let i = 1; i <= 1000; i++) {
    for (const t of [i / 1000, -i / 1000]) {
      const v = cost(t);
      if (v < bestCost) {
        best = t;
        bestCost = v;
      }
    }
  }
  return best;
}

function alphaFit(role: string, terminal: { bg: Rgb; fg: Rgb }): number {
  const c = parse(role);
  if (c[3] !== 255) return alphaOf(c[3]);
  return stepFit(terminal.bg, terminal.fg, [c[0], c[1], c[2]], terminal.bg);
}

// OKLab, tokens.ts only (spec §2.2 "Not primitives").
function oklab(c: Rgb): [number, number, number] {
  const lin = c.map((v) => {
    const x = v / 255;
    return x <= 0.04045 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4;
  });
  const l = Math.cbrt(0.4122214708 * lin[0]! + 0.5363325363 * lin[1]! + 0.0514459929 * lin[2]!);
  const m = Math.cbrt(0.2119034982 * lin[0]! + 0.6806995451 * lin[1]! + 0.1073969566 * lin[2]!);
  const s = Math.cbrt(0.0883024619 * lin[0]! + 0.2817188376 * lin[1]! + 0.6299787005 * lin[2]!);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}
export function deltaE(a: string, b: string): number {
  const x = oklab(rgb(a)),
    y = oklab(rgb(b));
  return Math.hypot(x[0] - y[0], x[1] - y[1], x[2] - y[2]);
}

const V10_PAIRS: [string, string][] = [
  ["status.needs_you", "status.failed"],
  ["status.needs_you", "status.done"],
  ["status.needs_you", "status.working"],
  ["status.failed", "status.done"],
  ["status.failed", "status.working"],
  ["status.done", "status.working"],
  ["accent.accent", "status.needs_you"],
];
export const at = (r: Roles, path: string): string =>
  path.split(".").reduce((o: any, k) => o[k], r);

export function oklabResults(r: Roles): Record<string, number> {
  return Object.fromEntries(
    V10_PAIRS.map(([a, b]) => [`${a}/${b}`, Math.round(deltaE(at(r, a), at(r, b)) * 1e4) / 1e4]),
  );
}
export function v10(r: Roles): Violation[] {
  return V10_PAIRS.flatMap(([a, b]) => {
    const d = deltaE(at(r, a), at(r, b));
    return d < 0.06
      ? [{ id: "V10", severity: "warning" as const, role: a, surface: b, value: d, bar: 0.06 }]
      : [];
  });
}

/** Least tint alpha (8-bit) for which text and dim read on glass over black and over white (V14). */
function tintMin(tint: Rgb, r: Roles, bar: number, lut: Float64Array): number {
  const ok = (A: number) =>
    [rgb(r.text.text), rgb(r.text.dim)].every((t) =>
      [BLACK, WHITE].every((b) => contrast(t, over([tint[0], tint[1], tint[2], A], b), lut) >= bar),
    );
  for (let A = 0; A <= 255; A++) if (ok(A)) return alphaOf(A);
  throw Error("glass: no tint alpha keeps text legible over a white backdrop");
}

export function fitParams(S: Sources, houseRoles: Record<Mode, Roles>): Params {
  const { motion_defaults: _defaults, ...constants } = S.deriveConstants;
  const lut = decodeLut(constants.srgb_lut);
  const P = { ...constants } as Params;
  P.layers = {} as Params["layers"];
  P.alpha = {} as Params["alpha"];
  P.scrim = {} as Params["scrim"];
  P.glass = { tint_min: {} as Record<Mode, number> };
  const nearer: string[] = [];
  for (const mode of MODES) {
    const t = S.house.terminal[mode],
      r = houseRoles[mode];
    const bg = rgb(t.bg),
      fg = rgb(t.fg),
      away = mode === "light" ? WHITE : BLACK;
    P.layers[mode] = {
      sidebar: stepFit(bg, fg, rgb(r.layer.sidebar), away),
      raised: stepFit(bg, fg, rgb(r.layer.raised), away),
      overlay: stepFit(bg, fg, rgb(r.layer.overlay), away),
    };
    const term = { bg, fg };
    P.alpha[mode] = {
      border: {
        muted: alphaFit(r.border.muted, term),
        base: alphaFit(r.border.base, term),
        strong: alphaFit(r.border.strong, term),
      },
      wash: {
        hover: alphaFit(r.wash.hover, term),
        press: alphaFit(r.wash.press, term),
        current: alphaFit(r.wash.current, term),
        selected: alphaFit(r.wash.selected, term),
      },
    };
    P.scrim[mode] = alphaOf(parse(r.layer.scrim)[3]);
    const sel = hex(rgb(r.wash.selected));
    nearer.push(deltaE(sel, r.accent.accent) < deltaE(sel, t.fg) ? "accent" : "fg");
    const glass = S.house.material.glass.tint[mode];
    P.glass.tint_min[mode] = tintMin(rgb(at(r, `layer.${glass.layer}`)), r, P.bars.text_floor, lut);
  }
  P.wash_selected_source = nearer.every((n) => n === "accent") ? "accent" : "fg";
  return P;
}

export function paramsSha256(P: Params): string {
  return createHash("sha256").update(canonical(P)).digest("hex");
}
function canonical(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map(canonical).join(",")}]`;
  if (v && typeof v === "object")
    return `{${Object.keys(v)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${canonical((v as any)[k])}`)
      .join(",")}}`;
  return JSON.stringify(v);
}
