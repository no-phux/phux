/**
 * Design tokens: load and validate crates/phux-design/tokens/src, fit the derive block from the
 * house look, run verify on house, reader and contrast_more (house errors throw), resolve the
 * computed roles and emit every token export: the ones phux commits at their repository paths,
 * and the mobile ones into the directory `design-gen --mobile-out` names (commands.ts).
 */
import {
  complete,
  derive,
  hex,
  rgb,
  withAlpha,
  type Mode,
  type Params,
  type Roles,
  type Violation,
} from "./derive.ts";
import { at, fitParams, oklabResults } from "./fit.ts";
import {
  MODES,
  coreOf,
  terminalTheme,
  validateCatalog,
  validateHouseColor,
  type CatalogTheme,
  type Sources,
} from "./sources.ts";
import { contrastMoreParams, solveContrastMore, solveReader } from "./variants.ts";
import { verify } from "./verify.ts";
export type { CatalogTheme, Sources } from "./sources.ts";

export interface Resolved {
  doc: any;
  params: Params;
  house: Record<Mode, Roles>;
  reader: Record<Mode, Roles>;
  contrastMore: Record<Mode, Roles>;
  catalog: {
    schema: number;
    source: string;
    aliases: Record<string, string>;
    legacy: any;
    themes: CatalogTheme[];
  };
  verify: { house: Violation[]; reader: Violation[]; contrast_more: Violation[] };
  terminalDiff: Record<Mode, { role: string; authored: string; derived: string }[]>;
}

function stripMode(r: Roles): Omit<Roles, "mode"> {
  const { mode: _mode, ...rest } = r;
  return rest;
}

/** Roles that derive(terminal) produces differently from the authored house (flat and companions follow). */
export function rolesDiff(
  a: Roles,
  b: Roles,
): { role: string; authored: string; derived: string }[] {
  const out: { role: string; authored: string; derived: string }[] = [];
  for (const group of [
    "layer",
    "text",
    "border",
    "wash",
    "accent",
    "status",
    "match",
    "diff",
    "selection",
  ] as const) {
    const ga = a[group] as Record<string, string>,
      gb = b[group] as Record<string, string>;
    for (const k of Object.keys(gb)) {
      if (group === "status" && /_(wash|edge|glow)$/.test(k)) continue;
      if (ga[k] !== gb[k])
        out.push({ role: `${group}.${k}`, authored: ga[k] ?? "", derived: gb[k]! });
    }
  }
  a.syntax.forEach((s, i) => {
    if (s !== b.syntax[i]) out.push({ role: `syntax.${i}`, authored: s, derived: b.syntax[i]! });
  });
  return out;
}

function houseExtras(S: Sources, mode: Mode, P: Params, r: Roles) {
  const g = S.house.material.glass.tint[mode];
  return {
    glass: { tint: at(r, `layer.${g.layer}`), alpha: g.alpha, tint_min: P.glass.tint_min[mode] },
    pulse_min_opacity: S.house.motion.loop.pulse.min_opacity,
    elevation: S.house.elevation[mode],
  };
}

function motion(S: Sources): any {
  const d = S.deriveConstants.motion_defaults,
    m = S.house.motion;
  return {
    spring: { ...d.spring, ...m.spring },
    curve: m.curve,
    loop: { ...d.loop, ...m.loop },
    enter_scale: m.enter_scale ?? d.enter_scale,
  };
}

function typeTokens(S: Sources): any {
  return S.house.type;
}

function elevation(S: Sources): any {
  const out: any = {};
  for (const mode of MODES)
    out[mode] = {
      ...S.house.elevation[mode],
      glow: {
        x: 0,
        y: 0,
        blur: 12,
        spread: 0,
        kind: "glow",
        alpha: S.deriveConstants.status_alpha.glow,
      },
    };
  return out;
}

function material(S: Sources, house: Record<Mode, Roles>, P: Params): any {
  const g = S.house.material.glass,
    m = S.house.material.metal;
  const tint: any = {},
    stops: any = {};
  for (const mode of MODES) {
    const t = g.tint[mode];
    tint[mode] = {
      layer: t.layer,
      alpha: t.alpha,
      color: hex(withAlpha(rgb(at(house[mode], `layer.${t.layer}`)), t.alpha)),
    };
    stops[mode] = m.stops[mode].map((s: any) => ({
      at: s.at,
      role: s.color,
      color: at(house[mode], s.color),
    }));
  }
  return {
    default: S.house.material.default,
    glass: { where: g.where, blur: g.blur, tint, tint_min: P.glass.tint_min, ring: g.ring },
    metal: { stops, rim: m.rim, sheen: m.sheen },
  };
}

export function houseCatalog(S: Sources): CatalogTheme[] {
  return MODES.map((mode) => {
    const t = S.house.terminal[mode];
    return {
      id: mode === "dark" ? "phux" : "phux-light",
      name: mode === "dark" ? "phux" : "phux Light",
      bg: t.bg,
      fg: t.fg,
      cursor: t.cursor,
      accent: null,
      selection_bg: t.selection_bg,
      dark: mode === "dark",
      ansi: t.ansi,
      ships: ["mobile", "rune", "tui"],
      icon: false,
    };
  });
}

export function resolveTokens(S: Sources): Resolved {
  for (const mode of MODES) validateHouseColor(mode, S.house.color[mode]);
  const themes = [...houseCatalog(S), ...validateCatalog(S.catalog)];
  const base = { ...S.deriveConstants } as Params;
  // Companions, match, diff and syntax only need the look-independent constants.
  const house = Object.fromEntries(
    MODES.map((m) => [m, complete(coreOf(m, S.house.color[m]), S.house.terminal[m].ansi, base)]),
  ) as Record<Mode, Roles>;
  const P = fitParams(S, house);
  const reader = Object.fromEntries(
    MODES.map((m) => [m, solveReader(S, m, house[m]!, P)]),
  ) as Record<Mode, Roles>;
  const contrastMore = Object.fromEntries(
    MODES.map((m) => [m, solveContrastMore(S, m, house[m]!, P)]),
  ) as Record<Mode, Roles>;
  const Q = contrastMoreParams(P);
  const results = {
    house: MODES.flatMap((m) =>
      verify(house[m], P, {
        context: "house",
        cursor: S.house.terminal[m].cursor,
        bg: S.house.terminal[m].bg,
        house: houseExtras(S, m, P, house[m]),
      }).map((v) => ({ ...v, mode: m })),
    ),
    reader: MODES.flatMap((m) =>
      verify(reader[m], P, { context: "house" }).map((v) => ({ ...v, mode: m })),
    ),
    contrast_more: MODES.flatMap((m) =>
      verify(contrastMore[m], Q, { context: "house", warn: ["V3"] }).map((v) => ({
        ...v,
        mode: m,
      })),
    ),
  };
  const terminalDiff = Object.fromEntries(
    MODES.map((m) => [m, rolesDiff(house[m], derive(terminalTheme(S.house.terminal[m], m), P))]),
  ) as Resolved["terminalDiff"];
  const vocabulary = { ...S.vocabulary.words, ...S.vocabulary.plurals };
  const doc = {
    $schema: "./phux.tokens.schema.json",
    schema: 2,
    design_version: S.version,
    house: S.house.key,
    house_name: S.house.name,
    color: Object.fromEntries(MODES.map((m) => [m, stripMode(house[m])])),
    terminal: S.house.terminal,
    variants: {
      reader: {
        color: Object.fromEntries(MODES.map((m) => [m, stripMode(reader[m])])),
        type: S.reader.type,
      },
      contrast_more: {
        color: Object.fromEntries(MODES.map((m) => [m, stripMode(contrastMore[m])])),
      },
      forced_colors: Object.fromEntries(
        Object.entries(S.forcedColors).filter(([k]) => k !== "source"),
      ),
    },
    vocabulary,
    status: S.status.ladder,
    type: typeTokens(S),
    radius: S.house.radius,
    space: S.house.space,
    icon: S.house.icon,
    layout: S.house.layout,
    haptic: S.house.haptic,
    elevation: elevation(S),
    motion: motion(S),
    material: material(S, house, P),
    derive: P,
    results: {
      oklab_delta_e: Object.fromEntries(MODES.map((m) => [m, oklabResults(house[m])])),
      verify: results,
      terminal_diff: terminalDiff,
    },
    identity: Object.fromEntries(Object.entries(S.identity).filter(([k]) => k !== "source")),
  };
  return {
    doc,
    params: P,
    house,
    reader,
    contrastMore,
    verify: results,
    terminalDiff,
    catalog: {
      schema: 1,
      source: S.catalog.source,
      aliases: S.catalog.aliases,
      legacy: S.catalog.legacy,
      themes,
    },
  };
}
