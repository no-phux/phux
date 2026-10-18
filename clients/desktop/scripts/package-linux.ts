/** Portable GNU/Linux bundle. No system install or platform-specific icon tools. */
import { chmodSync, cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { desktopRoot, repoRoot } from "./host.ts";
import { packageVersion, thirdPartyNotices } from "./package-app.ts";

const architectures: Record<string, { name: string; machine: number }> = {
  "x86_64-unknown-linux-gnu": { name: "x64", machine: 62 },
  "aarch64-unknown-linux-gnu": { name: "arm64", machine: 183 },
};

export function linuxAssetName(
  version: string,
  sha: string,
  target: string,
  channel: string,
): string {
  const arch = architectures[target];
  if (!arch) throw new Error(`Unsupported Linux desktop target: ${target}`);
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error("Invalid desktop version");
  if (!/^[0-9a-f]{40}$/.test(sha)) throw new Error("Invalid desktop build SHA");
  if (channel !== "stable" && channel !== "next") throw new Error("Invalid desktop channel");
  const id = channel === "next" ? `next.${sha}` : version;
  return `phux-desktop-${id}-linux-${arch.name}.tar.gz`;
}

interface LinuxBundleOptions {
  binary: string;
  output: string;
  version: string;
  sha: string;
  target: string;
  channel: "stable" | "next";
  notices: string;
}

function verifyLinuxExecutable(binary: string, target: string): void {
  const header = readFileSync(binary).subarray(0, 20);
  if (
    header.length < 20 ||
    header.toString("hex", 0, 4) !== "7f454c46" ||
    header[4] !== 2 ||
    header[5] !== 1 ||
    header.readUInt16LE(18) !== architectures[target]!.machine
  ) {
    throw new Error(`Desktop executable is not a 64-bit little-endian ELF for ${target}`);
  }
}

export function createLinuxBundle(options: LinuxBundleOptions): string {
  const { binary, output, version, sha, target, channel, notices } = options;
  const asset = linuxAssetName(version, sha, target, channel);
  verifyLinuxExecutable(binary, target);
  const bundle = join(output, "phux-desktop");
  rmSync(bundle, { recursive: true, force: true });
  const share = join(bundle, "share/phux-desktop");
  mkdirSync(join(bundle, "bin"), { recursive: true });
  mkdirSync(share, { recursive: true });
  cpSync(binary, join(bundle, "bin/phux-desktop"));
  chmodSync(join(bundle, "bin/phux-desktop"), 0o755);
  writeFileSync(
    join(share, "release.json"),
    JSON.stringify(
      {
        identifier: "dev.phux.desktop",
        version,
        sha,
        channel,
        target,
      },
      null,
      2,
    ) + "\n",
  );
  for (const name of ["LICENSE", "NOTICE"]) cpSync(join(repoRoot, name), join(share, name));
  writeFileSync(join(share, "THIRD-PARTY-NOTICES.txt"), notices);
  for (const name of ["PaperMono-OFL.txt", "JetBrainsMono-OFL.txt"]) {
    cpSync(join(desktopRoot, "native/assets/fonts", name), join(share, name));
  }
  mkdirSync(join(bundle, "share/applications"), { recursive: true });
  mkdirSync(join(bundle, "share/icons/hicolor/scalable/apps"), { recursive: true });
  cpSync(
    join(desktopRoot, "packaging/AppIcon.svg"),
    join(bundle, "share/icons/hicolor/scalable/apps/dev.phux.desktop.svg"),
  );
  writeFileSync(
    join(bundle, "share/applications/dev.phux.desktop.desktop"),
    `[Desktop Entry]
Type=Application
Name=Phux
Comment=Terminal control plane
Exec=phux-desktop
Icon=dev.phux.desktop
Terminal=false
Categories=Development;TerminalEmulator;
StartupWMClass=dev.phux.desktop
`,
  );
  return asset;
}

function main(): void {
  if (process.argv.length > 2) {
    throw new Error(
      "Linux packaging does not install builds; use the signed installer: curl -fsSL https://phux.sh/install | PHUX_REQUIRE_SIGNATURE=1 sh -s -- --desktop",
    );
  }
  const target =
    process.env.PHUX_DESKTOP_TARGET ??
    (process.arch === "arm64" ? "aarch64-unknown-linux-gnu" : "x86_64-unknown-linux-gnu");
  const git = Bun.spawnSync(["git", "-C", repoRoot, "rev-parse", "HEAD"]);
  if (git.exitCode !== 0) throw new Error(git.stderr.toString());
  const output = resolve(desktopRoot, "dist/linux");
  const asset = createLinuxBundle({
    binary:
      process.env.PHUX_DESKTOP_BIN ?? resolve(desktopRoot, "native/target/release/phux-desktop"),
    output,
    target,
    sha: git.stdout.toString().trim(),
    version: packageVersion(readFileSync(join(desktopRoot, "package.json"), "utf8")),
    channel: process.env.PHUX_CHANNEL === "next" ? "next" : "stable",
    notices: thirdPartyNotices(target),
  });
  const archive = resolve(desktopRoot, "dist", asset);
  const tar = Bun.spawnSync(["tar", "-czf", archive, "-C", output, "phux-desktop"]);
  if (tar.exitCode !== 0) throw new Error(tar.stderr.toString());
  console.log(`Built ${archive}`);
}

if (import.meta.main) main();
