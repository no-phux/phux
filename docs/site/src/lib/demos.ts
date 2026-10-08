import { getCollection } from "astro:content";
import { statSync } from "node:fs";
import { resolve, sep } from "node:path";

export async function getDemos() {
  const demos = await getCollection("demos");
  const publicRoot = resolve("public");
  for (const demo of demos) {
    for (const url of [demo.data.video, demo.data.poster, demo.data.captions].filter(Boolean)) {
      const asset = resolve(publicRoot, `.${url}`);
      if (!asset.startsWith(`${publicRoot}${sep}`)) throw new Error(`Demo ${demo.id}: asset must stay under public/: ${url}`);
      let info;
      try { info = statSync(asset); }
      catch { throw new Error(`Demo ${demo.id}: missing asset ${url}`); }
      if (!info.isFile() || info.size === 0 || info.size > 25 * 1024 * 1024) {
        throw new Error(`Demo ${demo.id}: ${url} must be a nonempty file of at most 25 MiB`);
      }
    }
  }
  return demos.sort((a, b) =>
    b.data.publishedAt.localeCompare(a.data.publishedAt) || a.id.localeCompare(b.id),
  );
}

const dateFormat = new Intl.DateTimeFormat("en", {
  month: "long",
  day: "numeric",
  year: "numeric",
  timeZone: "UTC",
});

export function demoDate(date: string): string {
  return dateFormat.format(new Date(`${date}T00:00:00Z`));
}

export function demoDuration(duration: number): string {
  const seconds = Math.ceil(duration);
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const remainder = String(seconds % 60).padStart(2, "0");
  return hours
    ? `${hours}:${String(minutes).padStart(2, "0")}:${remainder}`
    : `${minutes}:${remainder}`;
}
