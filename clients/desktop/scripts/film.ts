// Turn a PHUX_DESKTOP_FILM directory into an H.264 video.
//
//   bun scripts/film.ts FRAMES OUT.mp4 [--from MS] [--to MS] [--width PX]
//
// Frames are named by milliseconds since launch, so each one is held until
// the next frame's time: dropped frames keep real timing. Needs ffmpeg.
import { spawnSync } from "node:child_process";
import { readdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";

export interface Shot {
  file: string;
  at: number;
}

/** Frames in time order, kept within [from, to] milliseconds. */
export function shots(names: string[], from = 0, to = Infinity): Shot[] {
  return names
    .filter((name) => /^\d+\.png$/.test(name))
    .map((file) => ({ file, at: Number.parseInt(file, 10) }))
    .filter((shot) => shot.at >= from && shot.at <= to)
    .sort((left, right) => left.at - right.at);
}

/** An ffmpeg concat list that holds each frame until the next one. */
export function concatList(dir: string, frames: Shot[]): string {
  const lines: string[] = [];
  frames.forEach((shot, index) => {
    const next = frames[index + 1];
    lines.push(`file '${join(dir, shot.file)}'`);
    lines.push(`duration ${((next ? next.at - shot.at : 1_000) / 1_000).toFixed(3)}`);
  });
  // The concat demuxer ignores the last entry's duration unless it repeats.
  const last = frames.at(-1);
  if (last) lines.push(`file '${join(dir, last.file)}'`);
  return `${lines.join("\n")}\n`;
}

function option(args: string[], name: string): number | undefined {
  const index = args.indexOf(name);
  return index < 0 ? undefined : Number(args[index + 1]);
}

if (import.meta.main) {
  const args = process.argv.slice(2);
  const [frames, out] = args;
  if (!frames || !out) {
    console.error("usage: bun scripts/film.ts FRAMES OUT.mp4 [--from MS] [--to MS] [--width PX]");
    process.exit(2);
  }
  const dir = resolve(frames);
  const kept = shots(readdirSync(dir), option(args, "--from"), option(args, "--to"));
  if (kept.length === 0) throw new Error(`${dir}: no frames in range`);
  const list = join(dir, "film.txt");
  writeFileSync(list, concatList(dir, kept));
  const width = option(args, "--width") ?? 1920;
  const ffmpeg = spawnSync(
    "ffmpeg",
    [
      "-y",
      "-loglevel",
      "error",
      "-f",
      "concat",
      "-safe",
      "0",
      "-i",
      list,
      "-vf",
      `scale=${width}:-2:flags=lanczos,fps=30`,
      "-c:v",
      "libx264",
      "-pix_fmt",
      "yuv420p",
      "-crf",
      "20",
      "-movflags",
      "+faststart",
      resolve(out),
    ],
    { stdio: "inherit" },
  );
  process.exit(ffmpeg.status ?? 1);
}
