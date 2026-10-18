/**
 * Build `Phux.app`: the release GPUI executable, embedded-font license and
 * font files, the repository LICENSE and NOTICE with a third-party notice
 * generated from the desktop's Cargo graph, an icon rendered from
 * packaging/AppIcon.svg, and an ad-hoc signature. The executable is the Rust
 * binary. There is no JS bundle and no native addon.
 *
 * Usage: bun scripts/package-app.ts [--install]
 */
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { desktopBinary, desktopRoot, repoRoot } from "./host.ts";
import { renderNotices, shippedCrates } from "./notices.ts";

const root = desktopRoot;
const repo = repoRoot;
const app = resolve(root, "dist/app/Phux.app");
const contents = join(app, "Contents");
const binary = process.env.PHUX_DESKTOP_BIN ?? desktopBinary("release");
const fonts = resolve(root, "native/phux-gpui/assets/fonts");

if (import.meta.main) {
  if (!existsSync(binary)) {
    throw new Error(`Build the native host first (just desktop-native-build): ${binary}`);
  }
  rmSync(app, { recursive: true, force: true });
  mkdirSync(join(contents, "MacOS"), { recursive: true });
  mkdirSync(join(contents, "Resources/fonts"), { recursive: true });
  cpSync(binary, join(contents, "MacOS/phux-desktop"));
  run(["chmod", "755", join(contents, "MacOS/phux-desktop")]);
  for (const name of [
    "PaperMono-Regular.ttf",
    "PaperMono-Medium.ttf",
    "PaperMono-SemiBold.ttf",
    "PaperMono-Bold.ttf",
    "PaperMono-OFL.txt",
    "JetBrainsMonoNLNerdFontMono-Regular.ttf",
    "JetBrainsMonoNLNerdFontMono-Bold.ttf",
    "JetBrainsMonoNLNerdFontMono-Italic.ttf",
    "JetBrainsMonoNLNerdFontMono-BoldItalic.ttf",
    "JetBrainsMono-OFL.txt",
  ]) {
    cpSync(join(fonts, name), join(contents, "Resources/fonts", name));
  }
  for (const name of ["LICENSE", "NOTICE"]) {
    cpSync(join(repo, name), join(contents, "Resources", name));
  }
  writeFileSync(join(contents, "Resources/THIRD-PARTY-NOTICES.txt"), thirdPartyNotices());
  renderIcon(join(contents, "Resources/AppIcon.icns"));
  const version = packageVersion(readFileSync(join(root, "package.json"), "utf8"));
  const sha = run(["git", "-C", repo, "rev-parse", "HEAD"]).trim();
  writeFileSync(
    join(contents, "Info.plist"),
    infoPlist(version, sha, process.env.PHUX_CHANNEL === "next" ? "next" : "stable"),
  );
  writeFileSync(join(contents, "PkgInfo"), "APPL????");
  if (existsSync(join(contents, "Resources/phux-desktop-native.darwin-arm64.node"))) {
    throw new Error("Phux.app must not ship a NAPI addon");
  }
  run(["codesign", "--force", "--deep", "--sign", "-", app]);
  run(["codesign", "--verify", "--deep", "--strict", app]);
  console.log(`Built ${app} (${version}, ${sha})`);
  if (process.argv.includes("--install")) {
    const target = "/Applications/Phux.app";
    rmSync(target, { recursive: true, force: true });
    cpSync(app, target, { recursive: true });
    run(["/usr/bin/touch", target]);
    console.log(`Installed ${target}`);
  }
}

export function packageVersion(text: string): string {
  const metadata: unknown = JSON.parse(text);
  const version =
    metadata && typeof metadata === "object" && "version" in metadata
      ? metadata.version
      : undefined;
  if (typeof version !== "string" || !/^\d+\.\d+\.\d+$/.test(version)) {
    throw new Error("Desktop package.json must contain a stable release version");
  }
  return version;
}

export function infoPlist(
  version: string,
  sha: string,
  channel: "stable" | "next" = "stable",
): string {
  const numericVersion = version.split("-")[0] ?? version;
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>Phux</string>
  <key>CFBundleExecutable</key><string>phux-desktop</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleIdentifier</key><string>dev.phux.desktop</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>Phux</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${numericVersion}</string>
  <key>CFBundleVersion</key><string>${numericVersion}</string>
  <key>PhuxDesktopVersion</key><string>${version}</string>
  <key>PhuxDesktopChannel</key><string>${channel}</string>
  <key>PhuxDesktopHost</key><string>gpui</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>27.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>PhuxBuildSHA</key><string>${sha}</string>
</dict>
</plist>
`;
}

/**
 * Fails when any shipped crate declares no license, or ships no license text
 * and offers no license whose canonical text is in packaging/licenses.
 */
export function thirdPartyNotices(target = "aarch64-apple-darwin"): string {
  const metadata = run([
    "cargo",
    "metadata",
    "--locked",
    "--format-version",
    "1",
    "--filter-platform",
    target,
    "--manifest-path",
    resolve(root, "native/Cargo.toml"),
  ]);
  return renderNotices(shippedCrates(metadata));
}

function renderIcon(output: string): void {
  const work = join(tmpdir(), `phux-icon-${process.pid}`);
  const iconset = join(work, "AppIcon.iconset");
  mkdirSync(iconset, { recursive: true });
  run(["qlmanage", "-t", "-s", "1024", "-o", work, resolve(root, "packaging/AppIcon.svg")]);
  const master = join(work, "AppIcon.svg.png");
  for (const size of [16, 32, 128, 256, 512]) {
    run([
      "sips",
      "-z",
      `${size}`,
      `${size}`,
      master,
      "--out",
      join(iconset, `icon_${size}x${size}.png`),
    ]);
    const double = size * 2;
    run([
      "sips",
      "-z",
      `${double}`,
      `${double}`,
      master,
      "--out",
      join(iconset, `icon_${size}x${size}@2x.png`),
    ]);
  }
  run(["iconutil", "-c", "icns", iconset, "-o", output]);
  rmSync(work, { recursive: true, force: true });
}

function run(command: string[]): string {
  const result = Bun.spawnSync(command, { stdout: "pipe", stderr: "pipe" });
  if (result.exitCode !== 0) {
    throw new Error(`${command.join(" ")} failed: ${result.stderr.toString().trim()}`);
  }
  return result.stdout.toString();
}
