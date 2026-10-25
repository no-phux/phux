/** Native MSVC portable bundle. The signed ZIP is also the per-user installer's input. */
import { cpSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { desktopBrand } from "./brand.ts";
import { desktopRoot, repoRoot } from "./host.ts";
import { packageVersion, thirdPartyNotices } from "./package-app.ts";

export const WINDOWS_TARGET = "x86_64-pc-windows-msvc";

export function windowsAssetName(version: string, sha: string, channel: string): string {
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error("Invalid desktop version");
  if (!/^[0-9a-f]{40}$/.test(sha)) throw new Error("Invalid desktop build SHA");
  if (channel !== "stable" && channel !== "next") throw new Error("Invalid desktop channel");
  const identity = channel === "next" ? "next." + sha : version;
  return `phux-desktop-${identity}-windows-x64.zip`;
}

export function verifyWindowsExecutable(binary: string): void {
  const data = readFileSync(binary);
  if (data.length < 64 || data.toString("ascii", 0, 2) !== "MZ")
    throw new Error("Not a PE executable");
  const pe = data.readUInt32LE(0x3c);
  if (pe > data.length - 94 || data.toString("hex", pe, pe + 4) !== "50450000") {
    throw new Error("Invalid PE header");
  }
  if (data.readUInt16LE(pe + 4) !== 0x8664 || data.readUInt16LE(pe + 24) !== 0x20b) {
    throw new Error("Desktop executable must be AMD64 PE32+");
  }
  if (data.readUInt16LE(pe + 92) !== 2)
    throw new Error("Desktop executable must use the Windows GUI subsystem");
}

interface BundleOptions {
  binary: string;
  output: string;
  runtime: string;
  version: string;
  sha: string;
  channel: "stable" | "next";
  notices: string;
}

export function createWindowsBundle(options: BundleOptions): string {
  const { binary, output, runtime, version, sha, channel, notices } = options;
  const asset = windowsAssetName(version, sha, channel);
  verifyWindowsExecutable(binary);
  const dlls = readdirSync(runtime).filter((name) => name.toLowerCase().endsWith(".dll"));
  for (const required of ["vcruntime140.dll", "msvcp140.dll"]) {
    if (!dlls.includes(required)) throw new Error(`MSVC app-local runtime is missing ${required}`);
  }
  const bundle = join(output, "phux-desktop");
  rmSync(bundle, { recursive: true, force: true });
  const share = join(bundle, "share/phux-desktop");
  mkdirSync(share, { recursive: true });
  cpSync(binary, join(bundle, "phux-desktop.exe"));
  for (const name of dlls) cpSync(join(runtime, name), join(bundle, name));
  writeFileSync(
    join(share, "release.json"),
    JSON.stringify(
      {
        identifier: "dev.phux.desktop",
        brandProfile: desktopBrand().id,
        version,
        sha,
        channel,
        target: WINDOWS_TARGET,
      },
      null,
      2,
    ) + "\n",
  );
  for (const name of ["LICENSE", "NOTICE"]) cpSync(join(repoRoot, name), join(share, name));
  writeFileSync(join(share, "THIRD-PARTY-NOTICES.txt"), notices);
  for (const name of ["PaperMono-OFL.txt", "JetBrainsMono-OFL.txt"]) {
    cpSync(join(desktopRoot, "native/phux-gpui/assets/fonts", name), join(share, name));
  }
  cpSync(join(desktopRoot, "native/assets/fonts/Inter-OFL.txt"), join(share, "Inter-OFL.txt"));
  cpSync(join(repoRoot, "scripts/install-desktop-windows.ps1"), join(bundle, "install.ps1"));
  return asset;
}

export function archiveWindowsBundle(output: string, archive: string): void {
  const tar =
    process.platform === "win32"
      ? join(process.env.SystemRoot ?? "C:\\Windows", "System32/tar.exe")
      : "tar";
  const zip = Bun.spawnSync([tar, "--format=zip", "-cf", archive, "-C", output, "phux-desktop"]);
  if (zip.exitCode !== 0) throw new Error(zip.stderr.toString());
}

function main(): void {
  const runtime = process.env.PHUX_MSVC_RUNTIME_DIR;
  if (!runtime)
    throw new Error(
      "PHUX_MSVC_RUNTIME_DIR must name the x64 Microsoft.VC143.CRT redistribution directory",
    );
  const git = Bun.spawnSync(["git", "-C", repoRoot, "rev-parse", "HEAD"]);
  if (git.exitCode !== 0) throw new Error(git.stderr.toString());
  const output = resolve(desktopRoot, "dist/windows");
  const asset = createWindowsBundle({
    binary:
      process.env.PHUX_DESKTOP_BIN ??
      resolve(desktopRoot, "native/target", WINDOWS_TARGET, "release/phux-desktop.exe"),
    output,
    runtime,
    version: packageVersion(readFileSync(join(desktopRoot, "package.json"), "utf8")),
    sha: git.stdout.toString().trim(),
    channel: process.env.PHUX_CHANNEL === "next" ? "next" : "stable",
    notices: thirdPartyNotices(WINDOWS_TARGET),
  });
  // Use the OS bsdtar, not a GNU tar inherited from a Git/MSYS installation.
  const archive = resolve(desktopRoot, "dist", asset);
  archiveWindowsBundle(output, archive);
  console.log(`Built ${archive}`);
}

if (import.meta.main) main();
