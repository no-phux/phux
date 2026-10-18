// scripts/install.sh --desktop on macOS 27+ arm64 (skipped elsewhere): real
// zips, ad-hoc signatures and lockf locks behind a fake network. Run
// explicitly: `bun test ./tools/src/install/desktop.e2e.ts`.
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  symlinkSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";
import { capture } from "../lib/process.ts";
import { ROOT } from "../lib/repo.ts";
import { table, type Table } from "../lib/value.ts";
import { InstallFixture, json, sha256, VERSION } from "./fixture.ts";

const NEXT_SHA = "a".repeat(40);
const LOCK = ".phux-desktop-install.lock";
const LOCKED = "another Desktop install is publishing";

function macos27Arm64(): boolean {
  if (process.platform !== "darwin" || process.arch !== "arm64") return false;
  return Number(capture(["sw_vers", "-productVersion"]).split(".")[0]) >= 27;
}

const readPlist = (path: string): Table =>
  table(JSON.parse(capture(["plutil", "-convert", "json", "-o", "-", path])));

const executable = (path: string) => (statSync(path).mode & 0o111) !== 0;

/** Hold the installer's lock from another process until disposed. */
async function holdLock(lock: string): Promise<AsyncDisposable> {
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
    async [Symbol.asyncDispose]() {
      process.kill(-holder.pid, "SIGKILL");
      await holder.exited;
      rmSync(ready, { force: true });
    },
  };
}

class Fixture extends InstallFixture {
  readonly app = join(this.root, "source/Phux.app");
  plist: Record<string, string> = {
    CFBundleIdentifier: "dev.phux.desktop",
    CFBundleExecutable: "phux-desktop",
    CFBundlePackageType: "APPL",
    PhuxDesktopVersion: VERSION,
  };

  constructor() {
    super("Phux.app", `phux-desktop-${VERSION}-macos-arm64.zip`);
    mkdirSync(join(this.app, "Contents/MacOS"), { recursive: true });
    writeFileSync(join(this.root, "main.c"), "int main(void) { return 0; }\n");
    capture(["cc", join(this.root, "main.c"), "-o", join(this.app, "Contents/MacOS/phux-desktop")]);
    this.pack();
  }

  platform(): string[] {
    return ["--applications-dir", this.apps];
  }

  /** Sign and zip the app with `changes` to its Info.plist, and serve it. */
  pack(changes: Record<string, string> = {}): void {
    Object.assign(this.plist, changes);
    const info = join(this.app, "Contents/Info.plist");
    writeFileSync(info, JSON.stringify(this.plist));
    capture(["plutil", "-convert", "xml1", info]);
    capture(["codesign", "--force", "--sign", "-", this.app]);
    const zip = join(this.root, "app.zip");
    rmSync(zip, { force: true });
    capture(["ditto", "-c", "-k", "--keepParent", "--norsrc", this.app, zip]);
    this.serve(readFileSync(zip));
  }

  /** Serve a channel manifest from the golden fixture, pinning the served desktop asset. */
  manifest(channel: string, sha = "c".repeat(40), digest = this.digest): Table {
    const path = join(ROOT, `scripts/fixtures/release-manifest/${channel}.json`);
    const document = table(JSON.parse(readFileSync(path, "utf8")));
    const desktop = table(table(document["products"])["desktop"]);
    Object.assign(desktop, { version: VERSION, sha });
    table(table(desktop["artifacts"])["aarch64-apple-darwin"])["sha256"] = digest;
    writeFileSync(join(this.root, "manifest.json"), json(document));
    return document;
  }

  nextBuild(): void {
    this.asset = `phux-desktop-next.${NEXT_SHA}-macos-arm64.zip`;
    this.pack({ PhuxBuildSHA: NEXT_SHA, PhuxDesktopChannel: "next" });
    this.manifest("next", NEXT_SHA);
  }

  installedPlist(): Table {
    return readPlist(join(this.installed, "Contents/Info.plist"));
  }
}

describe.skipIf(!macos27Arm64())("install.sh --desktop on macOS", () => {
  let f: Fixture;
  beforeEach(() => {
    f = new Fixture();
  });
  afterEach(() => f.cleanup());

  test("install, update and launcher", () => {
    f.install();
    capture(["codesign", "--verify", "--deep", "--strict", f.installed]);
    f.pack({ TestBuild: "updated" });
    f.install();
    expect(f.installedPlist()["TestBuild"]).toBe("updated");
    expect(executable(join(f.launchers, "phux-desktop"))).toBe(true);
  });

  test("unpinned installs verify the manifest digest", () => {
    f.manifest("stable");
    unlinkSync(join(f.root, "SHA256SUMS"));
    expect(f.install(["--channel", "stable"]).stderr).toContain(`installed Phux ${VERSION}`);
    expect(f.calls.some((url) => url.includes("SHA256SUMS"))).toBe(false);
    const before = f.installedPlist();
    f.pack({ TestBuild: "must-not-install" });
    f.manifest("stable", undefined, "0".repeat(64));
    expect(f.install(["--channel", "stable"], false).stderr).toContain("checksum mismatch");
    expect(f.installedPlist()).toEqual(before);
  });

  test("next checks the bundle commit and channel; a revision pin reads no manifest", () => {
    f.nextBuild();
    expect(f.install(["--channel", "next"]).stderr).toContain(`installed Phux ${NEXT_SHA}`);
    f.pack({ PhuxBuildSHA: "b".repeat(40) });
    f.manifest("next", NEXT_SHA);
    expect(f.install(["--channel", "next"], false).stderr).toContain("is not commit");
    expect(f.installedPlist()["PhuxBuildSHA"]).toBe(NEXT_SHA);
    f.pack({ PhuxBuildSHA: NEXT_SHA, PhuxDesktopChannel: "stable" });
    const pinned = ["--channel", "next", "--revision", NEXT_SHA];
    expect(f.install(pinned, false).stderr).toContain("not a next build");
    f.pack({ PhuxDesktopChannel: "next" });
    unlinkSync(join(f.root, "manifest.json"));
    rmSync(join(f.root, "calls"));
    f.install(pinned);
    expect(f.calls.some((url) => url.includes("/channels/"))).toBe(false);
  });

  test("the bundle must be the requested stable release", () => {
    f.pack({ PhuxDesktopChannel: "next" });
    expect(f.install(undefined, false).stderr).toContain("not a stable build");
    f.pack({ PhuxDesktopChannel: "stable", PhuxDesktopVersion: "0.1.1" });
    expect(f.install(undefined, false).stderr).toContain("is not version");
    expect(existsSync(f.apps)).toBe(false);
  });

  test("a missing CLI is installed first, and a failed one installs no app", () => {
    unlinkSync(join(f.launchers, "phux"));
    const document = f.manifest("stable");
    f.install(["--channel", "stable"], false);
    expect(existsSync(f.installed)).toBe(false);
    const cli = join(f.root, "cli");
    mkdirSync(cli);
    for (const name of ["phux", "phux-mcp"])
      writeFileSync(join(cli, name), "#!/bin/sh\n", { mode: 0o755 });
    const target = "aarch64-apple-darwin";
    const pack = ["--id", "v9.8.7", "--target", target, "--bin-dir", cli, "--out-dir", f.root];
    capture(["bash", join(ROOT, "scripts/pack-release.sh"), ...pack]);
    renameSync(join(f.root, `phux-v9.8.7-${target}.tar.gz`), join(f.root, "cli.tar.gz"));
    const artifact = table(table(table(table(document["products"])["phux"])["artifacts"])[target]);
    artifact["sha256"] = sha256(readFileSync(join(f.root, "cli.tar.gz")));
    writeFileSync(join(f.root, "manifest.json"), json(document));
    const result = f.install(["--channel", "stable"]);
    expect(result.stderr).toContain("phux CLI not found; installing it first");
    expect(executable(join(f.launchers, "phux"))).toBe(true);
    expect(statSync(f.installed).isDirectory()).toBe(true);
  });

  test("staging touches nothing but the stage", () => {
    f.install();
    const before = f.installedPlist();
    const launcher = readFileSync(join(f.launchers, "phux-desktop"));
    unlinkSync(join(f.launchers, "phux"));
    const stage = join(f.apps, ".phux-desktop-update");
    for (const build of ["first", "second"]) {
      f.pack({ TestBuild: build });
      unlinkSync(join(f.root, "SHA256SUMS"));
      const result = f.run([
        "--stage-dir",
        `${stage}/`,
        "--version",
        VERSION,
        "--sha256",
        f.digest,
      ]);
      expect(result.stdout.trimEnd().split("\n").at(-1)).toBe(`staged: ${stage}/Phux.app`);
      expect(readPlist(join(stage, "Phux.app/Contents/Info.plist"))["TestBuild"]).toBe(build);
    }
    expect(readdirSync(stage)).toEqual(["Phux.app"]);
    expect(f.installedPlist()).toEqual(before);
    expect(readFileSync(join(f.launchers, "phux-desktop"))).toEqual(launcher);
    expect(existsSync(join(f.launchers, "phux"))).toBe(false);
  });

  test("staging refusals", async () => {
    f.nextBuild();
    const stage = join(f.apps, ".phux-desktop-update");
    const pinned = ["--stage-dir", stage, "--channel", "next", "--revision", NEXT_SHA];
    f.run([...pinned, "--sha256", f.digest]);
    const staged = () => readFileSync(join(stage, "Phux.app/Contents/Info.plist"));
    const before = staged();
    const refusals: [string[], string][] = [
      [["--stage-dir", "relative/dir", "--version", VERSION], "absolute directory"],
      [["--stage-dir", "/", "--version", VERSION], "absolute directory"],
      [["--stage-dir", stage, "--sha256", f.digest], "--sha256 needs"],
      [[...pinned, "--sha256", "0".repeat(64)], "checksum mismatch"],
    ];
    for (const [args, message] of refusals) {
      expect(f.run(args, false).stderr, args.join(" ")).toContain(message);
    }
    {
      await using _held = await holdLock(join(f.apps, LOCK));
      expect(f.run([...pinned, "--sha256", f.digest], false).stderr).toContain(LOCKED);
    }
    expect(staged()).toEqual(before);
  });

  test("a symlink in the archive is refused", () => {
    // Append a Phux.app/escape -> ../../outside entry to the signed archive.
    const extra = join(f.root, "symlink-entry");
    mkdirSync(join(extra, "Phux.app"), { recursive: true });
    symlinkSync("../../outside", join(extra, "Phux.app/escape"));
    capture(["zip", "-q", "-y", join(f.root, "app.zip"), "Phux.app/escape"], { cwd: extra });
    f.serve(readFileSync(join(f.root, "app.zip")));
    expect(f.install(undefined, false).stderr).toContain("symlinks");
    expect(existsSync(f.installed)).toBe(false);
  });

  test("a failed publish restores the previous app", () => {
    f.install();
    const before = f.installedPlist();
    f.pack({ TestBuild: "must-not-publish" });
    f.failPublish();
    f.install(undefined, false);
    expect(f.installedPlist()).toEqual(before);
    expect(f.leftovers()).toEqual([LOCK]);
  });

  test("an unrelated app is never replaced", () => {
    mkdirSync(f.installed, { recursive: true });
    writeFileSync(join(f.installed, "not-phux"), "keep me");
    expect(f.install(undefined, false).stderr).toContain("unrelated");
    expect(readFileSync(join(f.installed, "not-phux"), "utf8")).toBe("keep me");
  });

  test("a dead lock holder does not block and a live one does", async () => {
    f.install();
    const lock = join(f.apps, LOCK);
    // The holder (lockf and the child it runs) is SIGKILLed mid-hold.
    const owner = Bun.spawn(["lockf", "-k", lock, "sh", "-c", "kill -KILL $PPID $$"]);
    await owner.exited;
    expect(owner.signalCode).toBe("SIGKILL");
    f.install();
    await using _held = await holdLock(lock);
    expect(f.install(undefined, false).stderr).toContain(LOCKED);
  });
});
