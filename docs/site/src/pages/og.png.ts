import type { APIRoute } from "astro";
import sharp from "sharp";
import { SITE } from "../lib/site";

const entities: Record<string, string> = {
  "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&apos;",
};
const xml = (value: string) => value.replace(/[&<>"']/g, (char) => entities[char]);

// A static build asset, not a screenshot that retains yesterday's headline.
export const GET: APIRoute = async () => {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630" viewBox="0 0 1200 630">
    <rect width="1200" height="630" fill="#10151d"/>
    <g font-family="sans-serif">
      <text x="72" y="112" fill="#edf1f7" font-size="56" font-weight="700">${xml(SITE.name)}</text>
      <path d="M72 162H1128" stroke="#344052"/>
      <text x="72" y="262" fill="#a6b9ef" font-size="28">${xml(SITE.category)}</text>
      <text x="72" y="354" fill="#edf1f7" font-size="64" font-weight="700">${xml(SITE.tagline)}</text>
      <text x="72" y="552" fill="#aeb9c9" font-size="26">${xml(SITE.domain)}</text>
    </g>
  </svg>`;
  const image = await sharp(Buffer.from(svg)).png().toBuffer();
  return new Response(image, {
    headers: { "content-type": "image/png" },
  });
};
