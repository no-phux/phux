/**
 * tokens/css/phux-tokens.css for the website, phux-web and phux-org: `--phux-*` custom properties
 * for both modes and every accessibility mode, plus an `@theme inline` block for Tailwind v4.
 */
import type { Mode, Roles } from "../derive.ts";
import type { Resolved, Sources } from "../tokens.ts";
import { HEADER } from "./model.ts";

const kebab = (s: string) => s.replaceAll("_", "-").replaceAll(".", "-");
const px = (n: number) => `${n}px`;
const num = (n: number) => String(Math.round(n * 1e4) / 1e4);
type Decls = [string, string][];

// --- springs (spec §1.8 "Platform conversion") --------------------------------------

export interface SpringSpec {
  duration: number;
  bounce: number;
}

/** Damping ratio and natural frequency of a SwiftUI (duration, bounce) spring, mass 1. */
export function springConstants(s: SpringSpec): { zeta: number; omega: number; stiffness: number } {
  const omega = (2 * Math.PI) / s.duration;
  const zeta = s.bounce >= 0 ? 1 - s.bounce : 1 / (1 + s.bounce);
  return { zeta, omega, stiffness: omega * omega };
}

/** Position x(t) of the unit step response, from 0 at rest to 1. */
export function springAt(s: SpringSpec, t: number): number {
  const { zeta, omega } = springConstants(s);
  if (Math.abs(zeta - 1) < 1e-12) return 1 - Math.exp(-omega * t) * (1 + omega * t);
  if (zeta < 1) {
    const wd = omega * Math.sqrt(1 - zeta * zeta);
    return (
      1 -
      Math.exp(-zeta * omega * t) * (Math.cos(wd * t) + ((zeta * omega) / wd) * Math.sin(wd * t))
    );
  }
  const root = Math.sqrt(zeta * zeta - 1),
    r1 = -omega * (zeta - root),
    r2 = -omega * (zeta + root);
  return 1 + (r2 * Math.exp(r1 * t) - r1 * Math.exp(r2 * t)) / (r1 - r2);
}

/** The first t after which |1 - x(t)| < 0.001 holds for good, sampled every 0.1 ms. */
export function settleTime(x: (t: number) => number, horizon: number): number {
  const step = 1e-4;
  let last = 0;
  for (let t = 0; t <= horizon; t += step) if (Math.abs(1 - x(t)) >= 0.001) last = t;
  return Math.round((last + step) * 1e4) / 1e4;
}

export function springSettle(s: SpringSpec): number {
  return settleTime((t) => springAt(s, t), s.duration * 20);
}

/** 24 evenly spaced samples of x(t) over the settle time, ending exactly at 1. */
export function springSamples(s: SpringSpec): number[] {
  const settle = springSettle(s);
  return Array.from({ length: 24 }, (_, i) =>
    i === 23 ? 1 : Math.round(springAt(s, (i * settle) / 23) * 1e4) / 1e4,
  );
}

const linear = (samples: number[]) => `linear(${samples.map(num).join(", ")})`;
const bezier = (b: number[]) => `cubic-bezier(${b.map(num).join(", ")})`;

// --- declarations -------------------------------------------------------------------

function colorDecls(r: Roles): Decls {
  const out: Decls = [];
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
  ] as const)
    for (const [k, v] of Object.entries(r[group] as Record<string, string>))
      out.push([`--phux-${group}-${kebab(k)}`, v]);
  r.syntax.forEach((c, i) => out.push([`--phux-syntax-${i}`, c]));
  // No flat forms: CSS composites the translucent roles itself.
  return out;
}

function shadow(layers: any[]): string {
  if (!layers.length) return "none";
  return layers
    .map((s) => {
      const a = Math.round(s.alpha * 255)
        .toString(16)
        .toUpperCase()
        .padStart(2, "0");
      return `${s.kind === "lit" ? "inset " : ""}${px(s.x)} ${px(s.y)} ${px(s.blur)} ${px(s.spread)} ${s.color}${a}`;
    })
    .join(", ");
}

function modeDecls(R: Resolved, mode: Mode, r: Roles): Decls {
  const d = R.doc,
    tint = d.material.glass.tint[mode];
  const out: Decls = [["color-scheme", mode], ...colorDecls(r)];
  for (const tier of ["flat", "raised", "floating", "overlay"])
    out.push([`--phux-elevation-${tier}`, shadow(d.elevation[mode][tier])]);
  out.push(
    ["--phux-glass-tint", tint.color],
    ["--phux-glass-fallback", `var(--phux-layer-${tint.layer})`],
  );
  const stops = d.material.metal.stops[mode]
    .map((s: any) => `${s.color} ${num(s.at * 100)}%`)
    .join(", ");
  out.push(["--phux-metal", `linear-gradient(180deg, ${stops})`]);
  return out;
}

/** Increased contrast: the solved colours, opaque glass and a flat accent instead of metal. */
function contrastDecls(R: Resolved, mode: Mode): Decls {
  const tint = R.doc.material.glass.tint[mode];
  return [
    ...colorDecls(R.contrastMore[mode]),
    ["--phux-glass-tint", R.contrastMore[mode].layer[tint.layer as "sidebar"]],
    ["--phux-metal", R.contrastMore[mode].accent.accent],
  ];
}

function staticDecls(R: Resolved): Decls {
  const d = R.doc,
    out: Decls = [["color-scheme", "light dark"]];
  const stack = (f: any) => f.stack.map((n: string) => (/[\s]/.test(n) ? `"${n}"` : n)).join(", ");
  out.push(
    ["--phux-font-ui", stack(d.type.family.ui)],
    ["--phux-font-mono", stack(d.type.family.mono)],
  );
  out.push([
    "--phux-font-ui-features",
    d.type.family.ui.features.map((f: string) => `"${f}"`).join(", ") || "normal",
  ]);
  for (const [k, w] of Object.entries(d.type.weights)) out.push([`--phux-weight-${k}`, String(w)]);
  for (const [role, t] of Object.entries<any>(d.type.role)) {
    out.push(
      [`--phux-type-${role}-family`, `var(--phux-font-${t.family})`],
      [`--phux-type-${role}-size`, `${t.size.web}rem`],
      [`--phux-type-${role}-line-height`, `${t.line_height.web}rem`],
      [`--phux-type-${role}-weight`, String(t.weight)],
      [`--phux-type-${role}-tracking`, `${t.tracking_em}em`],
      [
        `--phux-type-${role}-features`,
        t.features.length ? t.features.map((f: string) => `"${f}"`).join(", ") : "normal",
      ],
    );
  }
  for (const [k, v] of Object.entries<number>(d.radius)) out.push([`--phux-radius-${k}`, px(v)]);
  for (const [k, v] of Object.entries<any>(d.space)) {
    if (k === "focus_ring")
      out.push(
        ["--phux-focus-ring-width", px(v.width)],
        ["--phux-focus-ring-offset", px(v.offset)],
      );
    else
      out.push([`--phux-space-${kebab(k)}`, px(typeof v === "object" ? (v.web ?? v.desktop) : v)]);
  }
  for (const [k, v] of Object.entries<number>(d.icon.size)) out.push([`--phux-icon-${k}`, px(v)]);
  for (const [k, v] of Object.entries<number>(d.icon.button))
    out.push([`--phux-icon-button-${k}`, px(v)]);
  for (const [k, v] of Object.entries<number>(d.layout.content_max))
    out.push([`--phux-content-max-${kebab(k)}`, px(v)]);
  for (const [name, pair] of Object.entries<any>(d.motion.spring)) {
    out.push(
      [`--phux-motion-${name}-duration`, `${springSettle(pair.enter)}s`],
      [`--phux-motion-${name}-easing`, linear(springSamples(pair.enter))],
      [`--phux-motion-${name}-exit-duration`, `${springSettle(pair.exit)}s`],
      [`--phux-motion-${name}-exit-easing`, linear(springSamples(pair.exit))],
    );
  }
  for (const [name, c] of Object.entries<any>(d.motion.curve))
    out.push(
      [`--phux-motion-${name}-duration`, `${c.duration}s`],
      [`--phux-motion-${name}-easing`, bezier(c.cubic_bezier)],
    );
  out.push(
    ["--phux-motion-pulse-period", `${d.motion.loop.pulse.period}s`],
    ["--phux-motion-pulse-min-opacity", num(d.motion.loop.pulse.min_opacity)],
    ["--phux-motion-shimmer-period", `${d.motion.loop.shimmer.period}s`],
    ["--phux-motion-indeterminate-period", `${d.motion.loop.indeterminate.period}s`],
    ["--phux-motion-enter-scale", num(d.motion.enter_scale)],
  );
  for (const [k, v] of Object.entries<number>(d.material.glass.blur))
    out.push([`--phux-glass-blur-${kebab(k)}`, px(v)]);
  const g = d.elevation.dark.glow;
  out.push(["--phux-glow-blur", px(g.blur)], ["--phux-glow-alpha", num(g.alpha)]);
  return out;
}

function reducedMotion(R: Resolved): Decls {
  const fade = R.doc.motion.curve.fade,
    out: Decls = [];
  for (const name of Object.keys(R.doc.motion.spring)) {
    out.push(
      [`--phux-motion-${name}-duration`, `${fade.duration}s`],
      [`--phux-motion-${name}-easing`, bezier(fade.cubic_bezier)],
      [`--phux-motion-${name}-exit-duration`, `${fade.duration}s`],
      [`--phux-motion-${name}-exit-easing`, bezier(fade.cubic_bezier)],
    );
  }
  out.push(
    ["--phux-motion-recolor-duration", "0s"],
    ["--phux-motion-pulse-period", "0s"],
    ["--phux-motion-shimmer-period", "0s"],
    ["--phux-motion-indeterminate-period", "0s"],
    ["--phux-motion-enter-scale", "1"],
  );
  return out;
}

function forcedDecls(R: Resolved): Decls {
  const map = R.doc.variants.forced_colors,
    r = R.house.dark,
    out: Decls = [];
  for (const [group, values] of Object.entries<Record<string, string>>(map)) {
    if (group === "control") continue;
    for (const [k, v] of Object.entries(values)) out.push([`--phux-${group}-${kebab(k)}`, v]);
  }
  for (const t of Object.keys(r.status))
    if (/_(wash|edge|glow)$/.test(t))
      out.push([`--phux-status-${kebab(t)}`, t.endsWith("_edge") ? "CanvasText" : "transparent"]);
  r.syntax.forEach((_, i) => out.push([`--phux-syntax-${i}`, "CanvasText"]));
  for (const [k, v] of Object.entries<string>(map.control))
    out.push([`--phux-control-${kebab(k)}`, v]);
  for (const tier of ["flat", "raised", "floating", "overlay"])
    out.push([`--phux-elevation-${tier}`, "none"]);
  out.push(["--phux-glass-tint", "Canvas"], ["--phux-metal", "ButtonFace"]);
  return out;
}

function block(selector: string, decls: Decls, indent = ""): string {
  return `${indent}${selector} {\n${decls.map(([k, v]) => `${indent}  ${k}: ${v};`).join("\n")}\n${indent}}`;
}
const media = (query: string, body: string[]) => `@media ${query} {\n${body.join("\n")}\n}`;

const NOT_READER = ':not([data-design="reader"])';
/** Beats every mode selector above it: an id in :not() adds (1,0,0) without matching anything. */
const EVERY = (tag: string) =>
  `:is(:root, [data-theme], .dark, .light, [data-design]):not(#${tag})`;

export function emitCss({ R, S }: { R: Resolved; S: Sources }): string {
  const H = R.house,
    RD = R.reader,
    readerLead = S.reader.type.lead;
  const tailwind: Decls = [];
  for (const [k] of colorDecls(H.dark)) tailwind.push([`--color-${k.slice(2)}`, `var(${k})`]);
  for (const k of Object.keys(R.doc.radius))
    tailwind.push([`--radius-phux-${k}`, `var(--phux-radius-${k})`]);
  tailwind.push(
    ["--font-phux-ui", "var(--phux-font-ui)"],
    ["--font-phux-mono", "var(--phux-font-mono)"],
  );
  const readerType: Decls = [
    ["--phux-type-lead-size", `${readerLead.size.web}rem`],
    ["--phux-type-lead-line-height", `${readerLead.line_height.web}rem`],
  ];
  const parts = [
    `/* ${HEADER} */`,
    `/* phux design tokens ${S.version} (house look: ${R.doc.house}). Dark by default; the`,
    '   system appearance, [data-theme] and .dark/.light pick the mode; [data-design="reader"] is the docs reader. */',
    "",
    block(":root", [
      ...staticDecls(R),
      ...modeDecls(R, "dark", H.dark).filter(([k]) => k !== "color-scheme"),
    ]),
    "",
    media("(prefers-color-scheme: light)", [
      block(':root:not([data-theme="dark"]):not(.dark)', modeDecls(R, "light", H.light), "  "),
    ]),
    "",
    block('[data-theme="dark"], .dark', modeDecls(R, "dark", H.dark)),
    "",
    block('[data-theme="light"], .light', modeDecls(R, "light", H.light)),
    "",
    media("(prefers-contrast: more)", [
      block(`:root${NOT_READER}`, contrastDecls(R, "dark"), "  "),
      block(`[data-theme="dark"]${NOT_READER}, .dark${NOT_READER}`, contrastDecls(R, "dark"), "  "),
      block(
        `[data-theme="light"]${NOT_READER}, .light${NOT_READER}`,
        contrastDecls(R, "light"),
        "  ",
      ),
    ]),
    "",
    media("(prefers-contrast: more) and (prefers-color-scheme: light)", [
      block(
        `:root${NOT_READER}:not([data-theme="dark"]):not(.dark)`,
        contrastDecls(R, "light"),
        "  ",
      ),
    ]),
    "",
    block('[data-design="reader"]', [...modeDecls(R, "dark", RD.dark), ...readerType]),
    "",
    media("(prefers-color-scheme: light)", [
      block(
        '[data-design="reader"]:not([data-theme="dark"]):not(.dark)',
        modeDecls(R, "light", RD.light),
        "  ",
      ),
    ]),
    "",
    block(
      '[data-design="reader"][data-theme="dark"], [data-design="reader"].dark, .dark [data-design="reader"]',
      modeDecls(R, "dark", RD.dark),
    ),
    "",
    block(
      '[data-design="reader"][data-theme="light"], [data-design="reader"].light, .light [data-design="reader"]',
      modeDecls(R, "light", RD.light),
    ),
    "",
    media("(prefers-reduced-transparency: reduce)", [
      block(
        EVERY("phux-reduced-transparency"),
        [
          ["--phux-glass-tint", "var(--phux-glass-fallback)"],
          ...Object.keys(R.doc.material.glass.blur).map((k): [string, string] => [
            `--phux-glass-blur-${kebab(k)}`,
            "0px",
          ]),
        ],
        "  ",
      ),
    ]),
    "",
    media("(prefers-reduced-motion: reduce)", [block(":root", reducedMotion(R), "  ")]),
    "",
    `@supports not (transition-timing-function: linear(0, 1)) {\n${block(
      ":root",
      Object.keys(R.doc.motion.spring).flatMap((n): Decls => [
        [`--phux-motion-${n}-easing`, bezier(R.doc.motion.curve.fade.cubic_bezier)],
        [`--phux-motion-${n}-exit-easing`, bezier(R.doc.motion.curve.fade.cubic_bezier)],
      ]),
      "  ",
    )}\n}`,
    "",
    media("(forced-colors: active)", [block(EVERY("phux-forced-colors"), forcedDecls(R), "  ")]),
    "",
    block("@theme inline", tailwind),
    "",
  ];
  return parts.join("\n");
}
