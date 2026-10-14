import type { APIRoute } from "astro";
import sharp from "sharp";
import foxMark from "../../public/favicon.svg?raw";
import { SITE } from "../lib/site";

const entities: Record<string, string> = {
  "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&apos;",
};
const xml = (value: string) => value.replace(/[&<>"']/g, (char) => entities[char]);

// Terminal-surface palette (src/styles/terminal-surface.css) plus the mark's lime.
const C = {
  bg: "#0b0b0d",
  surface: "#101013",
  raised: "#19191e",
  rule: "#29292e",
  fg: "#ededee",
  muted: "#aaaab1",
  dim: "#5c5c66",
  faint: "#34343c",
  live: "#bef264",
  ok: "#9ece6a",
  ask: "#e0af68",
  blue: "#7aa2f7",
  cyan: "#7dcfff",
};
const SANS = `'IBM Plex Sans', 'Helvetica Neue', Helvetica, Arial, 'DejaVu Sans', sans-serif`;

// Pane text is drawn as token bars, never characters: the split reads as live
// terminals at thumbnail size and cannot leak anything that looks like a session.
// A line is a prompt (">"), a cursor ("_"), and [width, color] tokens; [] is blank.
type Line = ([width: number, color: string] | ">" | "_")[];

function pane(x: number, y: number, lines: Line[]): string {
  return lines
    .map((line, row) => {
      const mid = y + row * 26;
      let cursor = x;
      return line
        .map((token) => {
          const at = cursor;
          if (token === ">") {
            cursor += 20;
            return `<path d="M${at} ${mid - 6}l8 6-8 6" fill="none" stroke="${C.live}" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>`;
          }
          if (token === "_") return `<rect x="${at}" y="${mid - 9}" width="10" height="18" fill="${C.fg}" opacity="0.75"/>`;
          cursor += token[0] + 9;
          return `<rect x="${at}" y="${mid - 4.5}" width="${token[0]}" height="9" rx="4.5" fill="${token[1]}"/>`;
        })
        .join("");
    })
    .join("");
}

// Desktop window: title bar, session sidebar, a three-way split, status bar.
const W = { x: 556, y: 66, w: 600, h: 498 };
const BAR = 44;
const STATUS = 38;
const SIDE = 150;
const main = { x: W.x + SIDE, y: W.y + BAR, w: W.w - SIDE, h: W.h - BAR - STATUS };
const splitX = main.x + Math.round(main.w * 0.54);
const splitY = main.y + Math.round(main.h * 0.5);
const right = main.x + main.w;

function sessionRow(y: number, dot: string, width: number, active = false): string {
  return `${active ? `<rect x="${W.x + 10}" y="${y - 17}" width="${SIDE - 20}" height="34" rx="8" fill="${C.raised}"/>` : ""}
    <circle cx="${W.x + 28}" cy="${y}" r="5" fill="${dot}"/>
    <rect x="${W.x + 44}" y="${y - 4.5}" width="${width}" height="9" rx="4.5" fill="${active ? C.muted : C.faint}"/>`;
}

// The shared fox mark, re-rooted as a nested SVG inside a rounded tile.
const MARK = { x: 80, y: 80, size: 100 };
const markSvg = foxMark
  .replace(/<\?xml[^>]*>/, "")
  .replace(/<title[\s\S]*?<\/title>|<desc[\s\S]*?<\/desc>/g, "")
  .replace(/<svg[^>]*>/, `<svg x="${MARK.x}" y="${MARK.y}" width="${MARK.size}" height="${MARK.size}" viewBox="0 0 512 512">`);

// Two balanced lines: the break that keeps the longer line shortest.
const words = SITE.tagline.split(" ");
const longerLine = (at: number) =>
  Math.max(words.slice(0, at).join(" ").length, words.slice(at).join(" ").length);
let breakAt = 1;
for (let at = 2; at < words.length; at++) if (longerLine(at) < longerLine(breakAt)) breakAt = at;
const tagline = [words.slice(0, breakAt).join(" "), words.slice(breakAt).join(" ")];

// A static build asset, not a screenshot that retains yesterday's headline.
export const GET: APIRoute = async () => {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630" viewBox="0 0 1200 630">
  <defs>
    <linearGradient id="window" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#141418"/>
      <stop offset="1" stop-color="#0e0e11"/>
    </linearGradient>
    <clipPath id="mark-clip"><rect x="${MARK.x}" y="${MARK.y}" width="${MARK.size}" height="${MARK.size}" rx="22"/></clipPath>
    <clipPath id="window-clip"><rect x="${W.x}" y="${W.y}" width="${W.w}" height="${W.h}" rx="14"/></clipPath>
    <filter id="glow" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="90"/></filter>
    <filter id="shadow" x="-20%" y="-20%" width="140%" height="150%">
      <feDropShadow dx="0" dy="18" stdDeviation="22" flood-color="#000" flood-opacity="0.6"/>
    </filter>
  </defs>

  <rect width="1200" height="630" fill="${C.bg}"/>
  <ellipse cx="${W.x + W.w / 2}" cy="${W.y + W.h / 2}" rx="330" ry="230" fill="${C.live}" opacity="0.07" filter="url(#glow)"/>

  <!-- identity -->
  <g clip-path="url(#mark-clip)">${markSvg}</g>
  <rect x="${MARK.x + 0.5}" y="${MARK.y + 0.5}" width="${MARK.size - 1}" height="${MARK.size - 1}" rx="21.5" fill="none" stroke="${C.rule}"/>
  <text x="${MARK.x - 5}" y="314" fill="${C.fg}" font-family="${SANS}" font-size="124" font-weight="700" letter-spacing="-4">${xml(SITE.name)}</text>
  <g font-family="${SANS}" font-size="37" font-weight="500" fill="${C.muted}" letter-spacing="-0.5">
    <text x="${MARK.x}" y="380">${xml(tagline[0])}</text>
    <text x="${MARK.x}" y="428">${xml(tagline[1])}</text>
  </g>
  <circle cx="${MARK.x + 6}" cy="533" r="6" fill="${C.live}"/>
  <text x="${MARK.x + 24}" y="542" fill="${C.muted}" font-family="${SANS}" font-size="26" font-weight="500">${xml(SITE.domain)}</text>

  <!-- desktop window -->
  <rect x="${W.x}" y="${W.y}" width="${W.w}" height="${W.h}" rx="14" fill="url(#window)" filter="url(#shadow)"/>
  <g clip-path="url(#window-clip)">
    <rect x="${W.x}" y="${W.y}" width="${W.w}" height="${BAR}" fill="${C.surface}"/>
    <g fill="${C.faint}">
      <circle cx="${W.x + 22}" cy="${W.y + 22}" r="6"/>
      <circle cx="${W.x + 42}" cy="${W.y + 22}" r="6"/>
      <circle cx="${W.x + 62}" cy="${W.y + 22}" r="6"/>
    </g>
    <rect x="${W.x + W.w / 2 - 52}" y="${W.y + 17.5}" width="104" height="9" rx="4.5" fill="${C.faint}"/>
    <path d="M${W.x} ${main.y - 0.5}H${W.x + W.w}" stroke="${C.rule}"/>

    <!-- sessions, grouped by host -->
    <rect x="${W.x}" y="${main.y}" width="${SIDE}" height="${main.h + STATUS}" fill="${C.surface}"/>
    <path d="M${main.x - 0.5} ${main.y}V${W.y + W.h}" stroke="${C.rule}"/>
    <rect x="${W.x + 20}" y="${main.y + 22}" width="54" height="7" rx="3.5" fill="${C.dim}"/>
    ${sessionRow(main.y + 66, C.live, 66, true)}
    ${sessionRow(main.y + 108, C.ask, 52)}
    ${sessionRow(main.y + 150, C.ok, 74)}
    ${sessionRow(main.y + 192, C.dim, 44)}
    <rect x="${W.x + 20}" y="${main.y + 248}" width="44" height="7" rx="3.5" fill="${C.dim}"/>
    ${sessionRow(main.y + 292, C.cyan, 60)}
    ${sessionRow(main.y + 334, C.dim, 48)}

    <!-- panes -->
    <rect x="${main.x}" y="${main.y}" width="${main.w}" height="${main.h}" fill="${C.bg}"/>
    <path d="M${splitX + 0.5} ${main.y}V${main.y + main.h}M${splitX} ${splitY + 0.5}H${right}" stroke="${C.rule}"/>
    ${pane(main.x + 22, main.y + 34, [
      [">", [44, C.fg], [70, C.blue]],
      [[94, C.dim]],
      [[132, C.dim]],
      [[78, C.dim]],
      [[110, C.dim]],
      [[54, C.ok], [38, C.dim]],
      [],
      [">", [30, C.fg], [58, C.cyan]],
      [[118, C.dim]],
      [[86, C.dim]],
      [[124, C.dim]],
      [],
      [">", "_"],
    ])}
    ${pane(splitX + 22, main.y + 34, [
      [">", [36, C.fg], [46, C.blue]],
      [[120, C.dim]],
      [[96, C.dim]],
      [[110, C.dim]],
      [[64, C.ok]],
    ])}
    ${pane(splitX + 22, splitY + 34, [
      [">", [52, C.fg]],
      [[128, C.dim]],
      [[104, C.dim]],
    ])}
    <!-- an agent waiting on a person: the one cue that is app UI, not a terminal row -->
    <rect x="${splitX + 18}" y="${splitY + 112}" width="${right - splitX - 36}" height="56" rx="10" fill="#1d1810" stroke="#5a4626"/>
    <circle cx="${splitX + 40}" cy="${splitY + 140}" r="6" fill="${C.ask}"/>
    <rect x="${splitX + 56}" y="${splitY + 135.5}" width="56" height="9" rx="4.5" fill="${C.ask}" opacity="0.8"/>
    <rect x="${right - 92}" y="${splitY + 126}" width="56" height="28" rx="7" fill="${C.ask}"/>
    <path d="M${right - 71} ${splitY + 140}l5 5 9-10" fill="none" stroke="#1d1810" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>

    <!-- focused pane -->
    <rect x="${main.x + 1.5}" y="${main.y + 1.5}" width="${splitX - main.x - 3}" height="${main.h - 3}" fill="none" stroke="${C.live}" stroke-opacity="0.55" stroke-width="2"/>

    <!-- status bar -->
    <rect x="${main.x}" y="${main.y + main.h}" width="${main.w}" height="${STATUS}" fill="${C.surface}"/>
    <path d="M${main.x} ${main.y + main.h + 0.5}H${right}" stroke="${C.rule}"/>
    <rect x="${main.x + 18}" y="${main.y + main.h + 12}" width="48" height="14" rx="7" fill="${C.live}" opacity="0.9"/>
    <rect x="${main.x + 78}" y="${main.y + main.h + 14.5}" width="40" height="9" rx="4.5" fill="${C.faint}"/>
    <rect x="${main.x + 130}" y="${main.y + main.h + 14.5}" width="52" height="9" rx="4.5" fill="${C.faint}"/>
    <circle cx="${right - 74}" cy="${main.y + main.h + 19}" r="5" fill="${C.ask}"/>
    <rect x="${right - 62}" y="${main.y + main.h + 14.5}" width="42" height="9" rx="4.5" fill="${C.faint}"/>
  </g>
  <rect x="${W.x + 0.5}" y="${W.y + 0.5}" width="${W.w - 1}" height="${W.h - 1}" rx="13.5" fill="none" stroke="#2f2f36"/>
</svg>`;
  const image = await sharp(Buffer.from(svg)).png().toBuffer();
  return new Response(image, {
    headers: { "content-type": "image/png" },
  });
};
