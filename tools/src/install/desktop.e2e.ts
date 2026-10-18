// scripts/install.sh --desktop on macOS arm64: real zips, real ad-hoc
// signatures and a real lockf lock, with a fake network.
//
// Run explicitly (`bun test ./tools/src/install/desktop.e2e.ts`); it is not
// part of the default `bun test` discovery, and skips off macOS 27+ arm64.
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  accessSync,
  chmodSync,
  constants,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  symlinkSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import { table, type Table } from "../lib/value.ts";

const INSTALLER = join(ROOT, "scripts/install.sh");
const GOLDEN = join(ROOT, "scripts/fixtures/release-manifest");
const VERSION = "0.1.0";
const SHELL = Bun.which("dash") ?? "/bin/sh";
const FAKE_CURL = String.raw`#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in -o) out="$2"; shift ;; https:*) url="$1" ;; esac
  shift
done
printf '%s\n' "$url" >> "$FIXTURE/calls"
case "$url" in
  */channels/*.json) file="$FIXTURE/manifest.json" ;;
  *SHA256SUMS | *.zip.sha256) file="$FIXTURE/SHA256SUMS" ;;
  *.zip) file="$FIXTURE/asset.zip" ;;
  *.tar.gz) file="$FIXTURE/cli.tar.gz" ;;
  *) echo "unexpected URL: $url" >&2; exit 1 ;;
esac
[ -f "$file" ] || exit 22
cp "$file" "$out"
`;

function macos27Arm64(): boolean {
  if (process.platform !== "darwin" || process.arch !== "arm64") return false;
  const version = Bun.spawnSync(["sw_vers", "-productVersion"], {
    stdout: "pipe",
  }).stdout.toString();
  return Number(version.split(".")[0]) >= 27;
}

/** Run a command that must succeed; returns stdout. */
function run(...argv: string[]): string {
  const result = Bun.spawnSync(argv, { stdout: "pipe", stderr: "pipe" });
  if (result.exitCode !== 0) {
    throw new Error(`${argv.join(" ")} exited ${result.exitCode}: ${result.stderr.toString()}`);
  }
  return result.stdout.toString();
}

function sha256File(path: string): string {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function xmlEscape(text: string): string {
  return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** An XML property list of string values, as plistlib.dumps writes one. */
function plistXml(values: Record<string, string>): string {
  const entries = Object.entries(values)
    .map(
      ([key, value]) => `\t<key>${xmlEscape(key)}</key>\n\t<string>${xmlEscape(value)}</string>\n`,
    )
    .join("");
  return (
    '<?xml version="1.0" encoding="UTF-8"?>\n' +
    '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n' +
    `<plist version="1.0">\n<dict>\n${entries}</dict>\n</plist>\n`
  );
}

function readPlist(path: string): Table {
  return table(JSON.parse(run("plutil", "-convert", "json", "-o", "-", path)));
}

function executable(path: string): boolean {
  try {
    accessSync(path, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

/** Hold the installer's lock from another process until `release` is called. */
async function holdLock(lock: string): Promise<{ release: () => Promise<void> }> {
  const ready = `${lock}.held`;
  // A group of its own, so release kills lockf and the child it runs.
  const holder = Bun.spawn(["lockf", "-k", lock, "sh", "-c", 'touch "$0"; exec sleep 60', ready], {
    stdio: ["ignore", "ignore", "ignore"],
    detached: true,
  });
  while (!existsSync(ready)) {
    if (holder.exitCode !== null) throw new Error("lock holder exited before taking the lock");
    await Bun.sleep(10);
  }
  return {
    release: async () => {
      process.kill(-holder.pid, "SIGKILL");
      await holder.exited;
      rmSync(ready, { force: true });
    },
  };
}

interface Result {
  readonly stdout: string;
  readonly stderr: string;
}

class Fixture {
  readonly root = mkdtempSync(join(tmpdir(), "phux-desktop-install-test-"));
  readonly fixture = join(this.root, "fixture");
  readonly bin = join(this.root, "bin");
  readonly apps = join(this.root, "Applications with spaces");
  readonly launchers = join(this.root, "launchers");
  readonly app = join(this.root, "source/Phux.app");
  readonly env: Record<string, string>;
  plist: Record<string, string> = {
    CFBundleIdentifier: "dev.phux.desktop",
    CFBundleExecutable: "phux-desktop",
    CFBundlePackageType: "APPL",
    PhuxDesktopVersion: VERSION,
  };
  asset = `phux-desktop-${VERSION}-macos-arm64.zip`;

  constructor() {
    for (const directory of [this.fixture, this.bin, this.launchers]) mkdirSync(directory);
    mkdirSync(join(this.app, "Contents/MacOS"), { recursive: true });
    const source = join(this.root, "main.c");
    writeFileSync(source, "int main(void) { return 0; }\n");
    run("cc", source, "-o", join(this.app, "Contents/MacOS/phux-desktop"));
    this.pack();
    writeFileSync(join(this.bin, "curl"), FAKE_CURL);
    chmodSync(join(this.bin, "curl"), 0o755);
    // A CLI is already installed unless a test removes it.
    symlinkSync("/usr/bin/true", join(this.launchers, "phux"));
    this.env = {
      HOME: join(this.root, "home"),
      FIXTURE: this.fixture,
      PATH: `${this.bin}:/usr/bin:/bin:/usr/sbin:/sbin`,
    };
  }

  cleanup(): void {
    rmSync(this.root, { recursive: true, force: true });
  }

  pack(): void {
    writeFileSync(join(this.app, "Contents/Info.plist"), plistXml(this.plist));
    run("codesign", "--force", "--sign", "-", this.app);
    const archive = join(this.fixture, "asset.zip");
    rmSync(archive, { force: true });
    run("ditto", "-c", "-k", "--keepParent", "--norsrc", this.app, archive);
    this.writeSums();
  }

  writeSums(): void {
    writeFileSync(join(this.fixture, "SHA256SUMS"), `${this.digest}  ${this.asset}\n`);
  }

  get digest(): string {
    return sha256File(join(this.fixture, "asset.zip"));
  }

  manifest(channel: string, sha = "c".repeat(40), digest?: string): void {
    const document = table(JSON.parse(readFileSync(join(GOLDEN, `${channel}.json`), "utf8")));
    const desktop = table(table(document["products"])["desktop"]);
    desktop["version"] = VERSION;
    desktop["sha"] = sha;
    table(table(desktop["artifacts"])["aarch64-apple-darwin"])["sha256"] = digest ?? this.digest;
    this.writeManifest(document);
  }

  writeManifest(document: Table): void {
    writeFileSync(join(this.fixture, "manifest.json"), `${JSON.stringify(document, null, 2)}\n`);
  }

  nextBuild(): string {
    const sha = "a".repeat(40);
    this.asset = `phux-desktop-next.${sha}-macos-arm64.zip`;
    Object.assign(this.plist, { PhuxBuildSHA: sha, PhuxDesktopChannel: "next" });
    this.pack();
    this.manifest("next", sha);
    return sha;
  }

  private invoke(args: string[], ok: boolean): Result {
    const result = Bun.spawnSync([SHELL, INSTALLER, "--desktop", ...args], {
      env: this.env,
      stdout: "pipe",
      stderr: "pipe",
    });
    const stdout = result.stdout.toString();
    const stderr = result.stderr.toString();
    expect(result.exitCode === 0, stdout + stderr).toBe(ok);
    return { stdout, stderr };
  }

  install(args: string[] = [], ok = true): Result {
    const scope = ["--applications-dir", this.apps, "--install-dir", this.launchers];
    return this.invoke([...scope, ...(args.length > 0 ? args : ["--version", VERSION])], ok);
  }

  stage(args: string[], ok = true): Result {
    return this.invoke(args, ok);
  }

  get installed(): string {
    return join(this.apps, "Phux.app");
  }

  installedPlist(): Table {
    return readPlist(join(this.installed, "Contents/Info.plist"));
  }

  get calls(): string[] {
    const path = join(this.fixture, "calls");
    return existsSync(path) ? readFileSync(path, "utf8").split("\n").filter(Boolean) : [];
  }
}

describe.skipIf(!macos27Arm64())(
  "install.sh --desktop (Phux Desktop installs on macOS 27+ arm64)",
  () => {
    let f: Fixture;
    beforeEach(() => {
      f = new Fixture();
    });
    afterEach(() => {
      f.cleanup();
    });

    test("install, update and launcher", () => {
      f.install();
      run("codesign", "--verify", "--deep", "--strict", f.installed);
      f.plist["TestBuild"] = "updated";
      f.pack();
      f.install();
      expect(f.installedPlist()["TestBuild"]).toBe("updated");
      expect(executable(join(f.launchers, "phux-desktop"))).toBe(true);
    });

    test("unpinned installs verify the manifest digest", () => {
      f.manifest("stable");
      unlinkSync(join(f.fixture, "SHA256SUMS"));
      expect(f.install(["--channel", "stable"]).stderr).toContain(`installed Phux ${VERSION}`);
      expect(f.calls.some((url) => url.includes("SHA256SUMS"))).toBe(false);
      const before = f.installedPlist();
      f.manifest("stable", undefined, "0".repeat(64));
      f.plist["TestBuild"] = "must-not-install";
      f.pack();
      expect(f.install(["--channel", "stable"], false).stderr).toContain("checksum mismatch");
      expect(f.installedPlist()).toEqual(before);
    });

    test("next checks the bundle commit and channel", () => {
      const sha = f.nextBuild();
      expect(f.install(["--channel", "next"]).stderr).toContain(`installed Phux ${sha}`);
      f.plist["PhuxBuildSHA"] = "b".repeat(40);
      f.pack();
      f.manifest("next", sha);
      expect(f.install(["--channel", "next"], false).stderr).toContain("is not commit");
      expect(f.installedPlist()["PhuxBuildSHA"]).toBe(sha);
      Object.assign(f.plist, { PhuxBuildSHA: sha, PhuxDesktopChannel: "stable" });
      f.pack();
      expect(f.install(["--channel", "next", "--revision", sha], false).stderr).toContain(
        "not a next build",
      );
    });

    test("a revision pin reads no manifest", () => {
      const sha = f.nextBuild();
      unlinkSync(join(f.fixture, "manifest.json"));
      f.install(["--channel", "next", "--revision", sha]);
      expect(f.calls.some((url) => url.includes("/channels/"))).toBe(false);
    });

    test("the bundle must be the requested stable release", () => {
      f.plist["PhuxDesktopChannel"] = "next";
      f.pack();
      expect(f.install([], false).stderr).toContain("not a stable build");
      Object.assign(f.plist, { PhuxDesktopChannel: "stable", PhuxDesktopVersion: "0.1.1" });
      f.pack();
      expect(f.install([], false).stderr).toContain("is not version");
      expect(existsSync(f.apps)).toBe(false);
    });

    test("a missing CLI is installed first", () => {
      unlinkSync(join(f.launchers, "phux"));
      const cli = join(f.root, "cli");
      mkdirSync(cli);
      for (const binary of ["phux", "phux-mcp"]) {
        writeFileSync(join(cli, binary), "#!/bin/sh\n");
        chmodSync(join(cli, binary), 0o755);
      }
      run(
        "bash",
        join(ROOT, "scripts/pack-release.sh"),
        "--id",
        "v9.8.7",
        "--target",
        "aarch64-apple-darwin",
        "--bin-dir",
        cli,
        "--out-dir",
        f.fixture,
      );
      const tarball = join(f.fixture, "cli.tar.gz");
      renameSync(join(f.fixture, "phux-v9.8.7-aarch64-apple-darwin.tar.gz"), tarball);
      f.manifest("stable");
      const document = table(JSON.parse(readFileSync(join(f.fixture, "manifest.json"), "utf8")));
      const phux = table(table(document["products"])["phux"]);
      table(table(phux["artifacts"])["aarch64-apple-darwin"])["sha256"] = sha256File(tarball);
      f.writeManifest(document);
      const result = f.install(["--channel", "stable"]);
      expect(result.stderr).toContain("phux CLI not found; installing it first");
      expect(executable(join(f.launchers, "phux"))).toBe(true);
      expect(statSync(f.installed).isDirectory()).toBe(true);
    });

    test("a failed CLI install installs no app", () => {
      unlinkSync(join(f.launchers, "phux"));
      f.manifest("stable");
      f.install(["--channel", "stable"], false);
      expect(existsSync(f.installed)).toBe(false);
    });

    test("staging touches nothing but the stage", () => {
      f.install();
      const before = f.installedPlist();
      const launcher = readFileSync(join(f.launchers, "phux-desktop"));
      unlinkSync(join(f.launchers, "phux"));
      unlinkSync(join(f.fixture, "SHA256SUMS"));
      const stage = join(f.apps, ".phux-desktop-update");
      for (const build of ["first", "second"]) {
        f.plist["TestBuild"] = build;
        f.pack();
        const args = ["--stage-dir", `${stage}/`, "--version", VERSION, "--sha256", f.digest];
        const result = f.stage(args);
        expect(result.stdout.trimEnd().split("\n").at(-1)).toBe(`staged: ${stage}/Phux.app`);
        expect(readPlist(join(stage, "Phux.app/Contents/Info.plist"))["TestBuild"]).toBe(build);
      }
      expect(readdirSync(stage).sort()).toEqual(["Phux.app"]);
      expect(f.installedPlist()).toEqual(before);
      expect(readFileSync(join(f.launchers, "phux-desktop"))).toEqual(launcher);
      expect(existsSync(join(f.launchers, "phux"))).toBe(false);
    });

    test("staging refusals", async () => {
      const sha = f.nextBuild();
      const stage = join(f.apps, ".phux-desktop-update");
      const pinned = ["--channel", "next", "--revision", sha];
      f.stage(["--stage-dir", stage, ...pinned, "--sha256", f.digest]);
      const staged = join(stage, "Phux.app/Contents/Info.plist");
      const before = readFileSync(staged);
      const refusals: [string[], string][] = [
        [["--stage-dir", "relative/dir", "--version", VERSION], "absolute directory"],
        [["--stage-dir", "/", "--version", VERSION], "absolute directory"],
        [["--stage-dir", stage, "--sha256", f.digest], "--sha256 needs"],
        [["--stage-dir", stage, ...pinned, "--sha256", "0".repeat(64)], "checksum mismatch"],
      ];
      for (const [args, message] of refusals) {
        expect(f.stage(args, false).stderr, args.join(" ")).toContain(message);
      }
      const holder = await holdLock(join(f.apps, ".phux-desktop-install.lock"));
      let result: Result;
      try {
        result = f.stage(["--stage-dir", stage, ...pinned, "--sha256", f.digest], false);
      } finally {
        await holder.release();
      }
      expect(result.stderr).toContain("another Desktop install is publishing");
      expect(readFileSync(staged)).toEqual(before);
    });

    test("a symlink in the archive is refused", () => {
      // Append a Phux.app/escape -> ../../outside entry to the signed archive.
      const extra = join(f.root, "symlink-entry");
      mkdirSync(join(extra, "Phux.app"), { recursive: true });
      symlinkSync("../../outside", join(extra, "Phux.app/escape"));
      const zip = Bun.spawnSync(
        ["zip", "-q", "-y", join(f.fixture, "asset.zip"), "Phux.app/escape"],
        {
          cwd: extra,
        },
      );
      expect(zip.exitCode).toBe(0);
      f.writeSums();
      expect(f.install([], false).stderr).toContain("symlinks");
      expect(existsSync(f.installed)).toBe(false);
    });

    test("a failed publish restores the previous app", () => {
      f.install();
      const before = f.installedPlist();
      f.plist["TestBuild"] = "must-not-publish";
      f.pack();
      writeFileSync(
        join(f.bin, "mv"),
        '#!/bin/sh\ncase "$1" in */.phux-desktop-install.*/Phux.app) exit 1 ;; esac\nexec /bin/mv "$@"\n',
      );
      chmodSync(join(f.bin, "mv"), 0o755);
      f.install([], false);
      expect(f.installedPlist()).toEqual(before);
      const leftovers = readdirSync(f.apps).filter((name) =>
        name.startsWith(".phux-desktop-install."),
      );
      expect(leftovers).toEqual([".phux-desktop-install.lock"]);
    });

    test("an unrelated app is never replaced", () => {
      mkdirSync(f.installed, { recursive: true });
      writeFileSync(join(f.installed, "not-phux"), "keep me");
      expect(f.install([], false).stderr).toContain("unrelated");
      expect(readFileSync(join(f.installed, "not-phux"), "utf8")).toBe("keep me");
    });

    test("a dead lock holder does not block and a live one does", async () => {
      f.install();
      const lock = join(f.apps, ".phux-desktop-install.lock");
      // The holder (lockf and the child it runs) is SIGKILLed mid-hold.
      const owner = Bun.spawn(["lockf", "-k", lock, "sh", "-c", "kill -KILL $PPID $$"], {
        stdio: ["ignore", "ignore", "ignore"],
      });
      await owner.exited;
      expect(owner.signalCode).toBe("SIGKILL");
      f.install();
      const holder = await holdLock(lock);
      try {
        expect(f.install([], false).stderr).toContain("another Desktop install is publishing");
      } finally {
        await holder.release();
      }
    });
  },
);
