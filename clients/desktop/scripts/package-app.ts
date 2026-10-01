/**
 * Build `Phux.app`: the shell and its launcher compiled into one Bun
 * executable (Contents/MacOS/phux-desktop), the release native addon beside it,
 * an icon rendered from packaging/AppIcon.svg, and an ad-hoc signature.
 *
 * Usage: bun scripts/package-app.ts [--install]   (--install copies to /Applications)
 */
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import solidPlugin from "@gpuix/solid/bun-plugin";
import { desktopFrameworkPlugin, prepareDesktopFramework } from "./desktop-bundle";

const root = resolve(import.meta.dir, "..");
const repo = resolve(root, "../..");
const app = resolve(root, "dist/app/Phux.app");
const contents = join(app, "Contents");
const addon = resolve(root, ".cache/host/phux-desktop-native.darwin-arm64.node");

if (!existsSync(addon))
  throw new Error(`Build the native host first (just desktop-native-build): ${addon}`);

rmSync(app, { recursive: true, force: true });
mkdirSync(join(contents, "MacOS"), { recursive: true });
mkdirSync(join(contents, "Resources"), { recursive: true });

prepareDesktopFramework();
const built = await Bun.build({
  entrypoints: [resolve(root, "scripts/app-main.ts")],
  target: "bun",
  minify: true,
  plugins: [desktopFrameworkPlugin, solidPlugin],
  compile: {
    target: "bun-darwin-arm64",
    outfile: join(contents, "MacOS/phux-desktop"),
    // A stray .env or bunfig.toml in the launch directory must not reconfigure the app.
    autoloadDotenv: false,
    autoloadBunfig: false,
  },
});
if (!built.success) throw new AggregateError(built.logs, "Desktop app compile failed");

cpSync(addon, join(contents, "Resources/phux-desktop-native.darwin-arm64.node"));
renderIcon(join(contents, "Resources/AppIcon.icns"));

const metadata: unknown = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const version =
  metadata && typeof metadata === "object" && "version" in metadata ? metadata.version : undefined;
if (typeof version !== "string" || !/^\d+\.\d+\.\d+(?:-alpha\.\d+)?$/.test(version))
  throw new Error("Desktop package.json must contain a release version");
const sha = run(["git", "-C", repo, "rev-parse", "HEAD"]).trim();
writeFileSync(join(contents, "Info.plist"), infoPlist(version, sha));
writeFileSync(join(contents, "PkgInfo"), "APPL????");
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

function infoPlist(version: string, sha: string): string {
  const numericVersion = version.split("-")[0];
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
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>27.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>PhuxBuildSHA</key><string>${sha}</string>
</dict>
</plist>
`;
}

function run(command: string[]): string {
  const result = Bun.spawnSync(command, { stdout: "pipe", stderr: "pipe" });
  if (result.exitCode !== 0) {
    throw new Error(`${command.join(" ")} failed: ${result.stderr.toString().trim()}`);
  }
  return result.stdout.toString();
}
