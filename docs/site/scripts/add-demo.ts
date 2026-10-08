#!/usr/bin/env bun
import {
  link, lstat, mkdir, mkdtemp, readFile, rename, rm, stat, writeFile,
} from "node:fs/promises";
import { join, resolve } from "node:path";
import { spawn } from "node:child_process";
import sharp from "sharp";

const SITE = resolve(import.meta.dirname, "..");
const ASSET_LIMIT = 25 * 1024 * 1024;
const USAGE = `Usage: bun run demo:add --file PATH --title TITLE --summary SUMMARY
  [--slug SLUG] [--captions PATH] [--version VERSION]

Run from docs/site. Requires ffmpeg and ffprobe on PATH.
Creates src/content/demos/<slug>.md and public/demos/<slug>/ without overwriting.`;

function options(args: string[]): Record<string, string> {
  const allowed: Record<string, true> = {
    file: true, title: true, summary: true, slug: true, captions: true, version: true,
  };
  const result: Record<string, string> = {};
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--" && i === 0) continue;
    const name = args[i]?.replace(/^--/, "");
    if (!args[i]?.startsWith("--") || !Object.hasOwn(allowed, name!)) {
      throw new Error(`Unknown option: ${args[i]}.\n${USAGE}`);
    }
    if (name! in result) throw new Error(`Option --${name} was supplied twice.`);
    const value = args[++i];
    if (!value?.trim() || value.startsWith("--")) {
      throw new Error(`Option --${name} requires a value.`);
    }
    result[name!] = value.trim();
  }
  for (const name of ["file", "title", "summary"]) {
    if (!result[name]) throw new Error(`Missing required --${name}.\n${USAGE}`);
  }
  return result;
}

async function run(program: string, args: string[]): Promise<string> {
  const { promise, resolve: resolveOutput, reject } = Promise.withResolvers<string>();
  const child = spawn(program, args, { stdio: ["ignore", "pipe", "pipe"] });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk: string) => { stdout += chunk; });
  child.stderr.on("data", (chunk: string) => { stderr = (stderr + chunk).slice(-8000); });
  child.on("error", (error: NodeJS.ErrnoException) => {
    reject(new Error(error.code === "ENOENT"
      ? `${program} is missing. Install ffmpeg (including ffprobe) and put it on PATH; only demo authoring needs it.`
      : `Could not start ${program}: ${error.message}`));
  });
  child.on("close", (code) => {
    if (code !== 0) reject(new Error(`${program} failed (${code}): ${stderr.trim()}`));
    else resolveOutput(stdout);
  });
  return await promise;
}

interface Probe {
  streams?: { index: number; codec_type?: string; width?: number; height?: number;
    disposition?: { attached_pic?: number } }[];
  format?: { duration?: string };
}

async function probe(file: string): Promise<Probe> {
  const data: Probe = JSON.parse(await run("ffprobe", [
    "-v", "error", "-show_entries",
    "stream=index,codec_type,width,height:stream_disposition=attached_pic:format=duration",
    "-of", "json", file,
  ]));
  const duration = Number(data.format?.duration);
  if (!Number.isFinite(duration) || duration <= 0) {
    throw new Error(`Video must have a finite positive recording duration: ${file}`);
  }
  return data;
}

async function fileSize(file: string): Promise<number> {
  let info;
  try { info = await stat(file); }
  catch { throw new Error(`Cannot read input file: ${file}`); }
  if (!info.isFile()) throw new Error(`Not a regular file: ${file}`);
  return info.size;
}

async function assertAbsent(path: string): Promise<void> {
  try { await lstat(path); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return;
    throw error;
  }
  throw new Error(`Refusing to overwrite existing path: ${path}. Choose another --slug.`);
}

async function checkAsset(file: string): Promise<void> {
  const size = await fileSize(file);
  if (size === 0 || size > ASSET_LIMIT) {
    throw new Error(`${file} is ${(size / 1024 / 1024).toFixed(2)} MiB; each published asset must be nonempty and at most 25 MiB. Trim the recording or reduce its resolution before adding it.`);
  }
}

function validateCaptions(text: string): void {
  const normalized = text.replace(/^\uFEFF/, "").replace(/\r\n?/g, "\n");
  const blocks = normalized.trim().split(/\n[ \t]*\n/);
  if (!/^WEBVTT(?:[ \t][^\n]*)?(?:\n|$)/.test(blocks[0] ?? "")) {
    throw new Error("Captions must be a UTF-8 WebVTT file beginning with WEBVTT (not SRT).");
  }
  const timestamp = "(?:\\d{2,}:)?[0-5]\\d:[0-5]\\d\\.\\d{3}";
  const timing = new RegExp(`^(${timestamp})[ \\t]+-->[ \\t]+(${timestamp})(?:[ \\t]+[^\\n]+)?$`);
  const seconds = (value: string) => value.split(":").reduce((total, part) => total * 60 + Number(part), 0);
  let cues = 0;
  for (const block of blocks.slice(1)) {
    if (/^(?:NOTE(?:[ \t]|\n|$)|STYLE(?:\n|$)|REGION(?:\n|$))/.test(block)) continue;
    const lines = block.split("\n");
    const index = lines[0]?.includes("-->") ? 0 : 1;
    const match = timing.exec(lines[index] ?? "");
    if (!match || seconds(match[2]!) <= seconds(match[1]!) || !lines.slice(index + 1).join("\n").trim()) {
      throw new Error("Invalid WebVTT cue: expected start --> end timestamps and caption text, separated from other cues by a blank line.");
    }
    cues++;
  }
  if (!cues) throw new Error("Captions must contain at least one WebVTT cue.");
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args.length === 1 && ["--help", "-h"].includes(args[0]!)) {
    console.log(USAGE);
    return;
  }
  const opts = options(args);
  const slug = opts.slug ?? opts.title!.normalize("NFKD")
    .replace(/[\u0300-\u036f]/g, "").toLowerCase()
    .replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
  if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(slug) || slug.length > 100) {
    throw new Error("Slug must be 1–100 lowercase ASCII letters/digits separated by single hyphens. Supply --slug if the title cannot produce one.");
  }
  const input = resolve(opts.file!);
  const assetsRoot = join(SITE, "public/demos");
  const assets = join(assetsRoot, slug);
  const entries = join(SITE, "src/content/demos");
  const entry = join(entries, `${slug}.md`);
  await assertAbsent(entry);
  await assertAbsent(assets);
  await fileSize(input);
  // Probe before creating even temporary output; attached cover art is not video.
  const source = await probe(input);
  const stream = source.streams?.find((s) => s.codec_type === "video"
    && !s.disposition?.attached_pic && (s.width ?? 0) > 0 && (s.height ?? 0) > 0);
  if (!stream) throw new Error(`Input contains no video stream: ${input}`);
  await run("ffmpeg", ["-version"]);
  let captions: string | undefined;
  if (opts.captions) {
    const path = resolve(opts.captions);
    await checkAsset(path);
    captions = await readFile(path, "utf8");
    validateCaptions(captions);
  }

  await mkdir(assetsRoot, { recursive: true });
  // Staging beside the destination keeps rename/link on the same filesystem.
  const staging = await mkdtemp(join(assetsRoot, ".add-demo-"));
  let installedAssets = false;
  let installedEntry = false;
  try {
    const stagedAssets = join(staging, "assets");
    await mkdir(stagedAssets);
    const video = join(stagedAssets, "video.mp4");
    const poster = join(stagedAssets, "poster.webp");
    // Fit the display aspect ratio (including non-square source pixels) into
    // 1920 × 1080 without upscaling; H.264 yuv420p needs even dimensions.
    const factor = "min(1,min(1920/(iw*sar),1080/ih))";
    const scale = `scale=w='max(2,trunc(iw*sar*${factor}/2)*2)':h='max(2,trunc(ih*${factor}/2)*2)',setsar=1`;
    await run("ffmpeg", [
      "-hide_banner", "-loglevel", "error", "-nostdin", "-n", "-i", input,
      "-map", `0:${stream.index}`, "-map", "0:a:0?", "-map_metadata", "-1", "-map_chapters", "-1",
      "-vf", scale, "-c:v", "libx264", "-preset", "medium", "-crf", "23",
      "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "128k",
      "-movflags", "+faststart", video,
    ]);
    await checkAsset(video);
    const encoded = await probe(video);
    const duration = Number(encoded.format?.duration);
    // Sample an early real segment; thumbnail chooses a representative frame
    // instead of blindly using a potentially black first frame.
    const posterFrame = join(staging, "poster.png");
    await run("ffmpeg", [
      "-hide_banner", "-loglevel", "error", "-nostdin", "-n",
      "-ss", String(duration >= 1 ? Math.min(1, duration / 10) : 0), "-i", video,
      "-t", String(Math.min(5, duration)),
      "-vf", "thumbnail=30,scale=w='min(1280,iw)':h=-2",
      "-frames:v", "1", posterFrame,
    ]);
    await sharp(posterFrame).webp({ quality: 85 }).toFile(poster);
    await checkAsset(poster);
    if (captions !== undefined) {
      await writeFile(join(stagedAssets, "captions.vtt"), captions, "utf8");
      await checkAsset(join(stagedAssets, "captions.vtt"));
    }
    const url = `/demos/${slug}`;
    const metadata = [
      "---", `title: ${JSON.stringify(opts.title)}`, `summary: ${JSON.stringify(opts.summary)}`,
      `publishedAt: ${JSON.stringify(new Date().toISOString().slice(0, 10))}`, `duration: ${duration}`,
      `video: ${JSON.stringify(`${url}/video.mp4`)}`, `poster: ${JSON.stringify(`${url}/poster.webp`)}`,
      ...(captions !== undefined ? [`captions: ${JSON.stringify(`${url}/captions.vtt`)}`] : []),
      ...(opts.version ? [`version: ${JSON.stringify(opts.version)}`] : []),
      "---", "", opts.summary!.replace(/\s+/g, " ").replace(/[\\`*_{}\[\]()#+.!>|~-]/g, "\\$&"), "",
    ].join("\n");
    const stagedEntry = join(staging, "entry.md");
    await writeFile(stagedEntry, metadata, "utf8");
    await mkdir(entries, { recursive: true });
    await assertAbsent(entry);
    // Exclusive mkdir reserves the slug, even if another author raced us.
    try { await mkdir(assets); }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code === "EEXIST") {
        throw new Error(`Refusing to overwrite existing path: ${assets}. Choose another --slug.`);
      }
      throw error;
    }
    installedAssets = true;
    await rename(stagedAssets, assets);
    // The entry becomes visible only after every asset is ready. Hard-link
    // installation is atomic and refuses an existing entry, unlike rename.
    try { await link(stagedEntry, entry); }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code === "EEXIST") {
        throw new Error(`Refusing to overwrite existing path: ${entry}. Choose another --slug.`);
      }
      throw error;
    }
    installedEntry = true;
  } finally {
    try {
      if (installedAssets && !installedEntry) await rm(assets, { recursive: true, force: true });
    } finally {
      await rm(staging, { recursive: true, force: true });
    }
  }
  console.log(`Added ${entry}\nAssets: ${assets}\nEdit the Markdown walkthrough, review the video/captions, then commit both paths to publish through the main site-deploy workflow.`);
}

try { await main(); }
catch (error) {
  console.error(`demo:add: ${error instanceof Error ? error.message : error}`);
  process.exitCode = 1;
}
