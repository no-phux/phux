// scripts/install.sh --desktop on Linux: installer transactions with real
// archives behind a fake network; runs on any POSIX host. Run explicitly:
// `bun test ./tools/src/install/desktop-linux.e2e.ts`.
import { afterEach, beforeEach, expect, test } from "bun:test";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  rmdirSync,
  statSync,
  symlinkSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { join, relative } from "node:path";
import { createLinuxBundle } from "../../../clients/desktop/scripts/package-linux.ts";
import { capture } from "../lib/process.ts";
import { InstallFixture, json, sha256, VERSION } from "./fixture.ts";

const TARGET = "x86_64-unknown-linux-gnu";
const SHA = "a".repeat(40);

/** A ustar entry; `type` is the typeflag: 0 file, 1 hardlink, 2 symlink, 5 dir, 6 fifo. */
interface Entry {
  readonly name: string;
  readonly type?: "0" | "1" | "2" | "5" | "6";
  readonly mode?: number;
  readonly content?: string;
  readonly linkname?: string;
}

/** A gzipped ustar archive, written by hand so hostile entries are exact. */
function tarGz(entries: readonly Entry[]): Uint8Array {
  const blocks = entries.flatMap(
    ({ name, type = "0", mode = 0o644, content = "", linkname = "" }) => {
      const body = new TextEncoder().encode(content);
      const header = new Uint8Array(512);
      const put = (offset: number, text: string) =>
        header.set(new TextEncoder().encode(text), offset);
      const octal = (offset: number, width: number, value: number) =>
        put(offset, value.toString(8).padStart(width - 1, "0"));
      put(0, name);
      octal(100, 8, mode);
      octal(108, 8, 0); // uid
      octal(116, 8, 0); // gid
      octal(124, 12, body.length);
      octal(136, 12, 0); // mtime
      put(148, " ".repeat(8)); // the checksum counts its own field as spaces
      put(156, type);
      put(157, linkname);
      put(257, "ustar\u000000");
      const checksum = header.reduce((sum, byte) => sum + byte, 0);
      put(148, `${checksum.toString(8).padStart(6, "0")}\0 `);
      const padded = new Uint8Array(Math.ceil(body.length / 512) * 512);
      padded.set(body);
      return [header, padded];
    },
  );
  return Bun.gzipSync(Buffer.concat([...blocks, new Uint8Array(1024)]));
}

class Fixture extends InstallFixture {
  metadata: Record<string, string> = {
    identifier: "dev.phux.desktop",
    version: VERSION,
    sha: SHA,
    channel: "stable",
    target: TARGET,
  };

  constructor() {
    super("phux-desktop", `phux-desktop-${VERSION}-linux-x64.tar.gz`);
    this.env["XDG_DATA_HOME"] = this.apps;
    this.pack();
  }

  platform(): string[] {
    return ["--os", "linux", "--arch", "x86_64"];
  }

  /** Serve a bundle with `changes` to its release.json, and any `extra` entry. */
  pack(changes: Record<string, string> = {}, extra?: Entry, executable?: Entry): void {
    Object.assign(this.metadata, changes);
    const launcher = '#!/bin/sh\nprintf "linux:%s\\n" "$*"\n';
    this.serve(
      tarGz([
        executable ?? { name: "phux-desktop/bin/phux-desktop", mode: 0o755, content: launcher },
        { name: "phux-desktop/share/phux-desktop/release.json", content: json(this.metadata) },
        ...(extra ? [extra] : []),
      ]),
    );
  }

  release(): string {
    return readFileSync(join(this.installed, "share/phux-desktop/release.json"), "utf8");
  }
}

let fx: Fixture;
beforeEach(() => {
  fx = new Fixture();
});
afterEach(() => fx.cleanup());

test("install, update and launcher", () => {
  fx.install();
  const launched = capture([join(fx.launchers, "phux-desktop"), "argument with spaces"]);
  expect(launched).toBe("linux:argument with spaces\n");
  fx.pack({ sha: "b".repeat(40) });
  fx.install();
  expect(JSON.parse(fx.release())).toMatchObject({ sha: "b".repeat(40) });
  expect(fx.leftovers()).toEqual([]);
});

test("arm64 next names", () => {
  fx.asset = `phux-desktop-next.${SHA}-linux-arm64.tar.gz`;
  fx.pack({ channel: "next", target: "aarch64-unknown-linux-gnu" });
  const result = fx.install(["--arch", "arm64", "--channel", "next", "--revision", SHA]);
  expect(result.stderr).toContain("installed Phux");
  expect(fx.calls.join("\n")).toContain(fx.asset);
});

test("manifest resolution", () => {
  const desktop = {
    version: VERSION,
    sha: SHA,
    protocol: "0.9",
    artifacts: { [TARGET]: { sha256: fx.digest } },
  };
  const manifest = { schema_version: 1, channel: "stable", products: { desktop } };
  writeFileSync(join(fx.root, "manifest.json"), json(manifest));
  fx.install(["--channel", "stable"]);
  expect(statSync(fx.installed).isDirectory()).toBe(true);
});

test("a checksum or required signature refuses without replacing", () => {
  fx.install();
  const before = fx.release();
  const mismatch = fx.install(["--version", VERSION, "--sha256", "0".repeat(64)], false);
  expect(mismatch.stderr).toContain("checksum mismatch");
  fx.env["PHUX_REQUIRE_SIGNATURE"] = "1";
  expect(fx.install(undefined, false).stderr).toMatch(/minisign|signature/);
  fx.script("minisign", "#!/bin/sh\nexit 1\n");
  writeFileSync(join(fx.root, "signature"), "foreign signature");
  fx.install(undefined, false);
  expect(fx.release()).toBe(before);
});

test("a directory is not a desktop executable", () => {
  fx.install();
  const executable = join(fx.installed, "bin/phux-desktop");
  const before = readFileSync(executable, "utf8");
  fx.pack({}, undefined, { name: "phux-desktop/bin/phux-desktop", type: "5", mode: 0o755 });
  expect(fx.install(undefined, false).stderr).toContain("no desktop executable");
  expect(readFileSync(executable, "utf8")).toBe(before);
});

test("a custom CLI on PATH is not reinstalled", () => {
  unlinkSync(join(fx.launchers, "phux"));
  symlinkSync("/usr/bin/true", join(fx.bin, "phux"));
  fx.install();
  expect(existsSync(join(fx.launchers, "phux"))).toBe(false);
});

test("identity, version, channel and target are checked", () => {
  const original = { ...fx.metadata };
  for (const [field, value] of [
    ["identifier", "dev.example.other"],
    ["version", "9.9.9"],
    ["target", "aarch64-unknown-linux-gnu"],
    ["channel", "next"],
  ] as const) {
    fx.pack({ ...original, [field]: value });
    fx.install(undefined, false);
    expect([field, existsSync(fx.installed)]).toEqual([field, false]);
  }
});

test("links, traversal and special files are refused before extraction", () => {
  const hostile: Entry[] = [
    { name: "phux-desktop/share", type: "2", linkname: fx.root },
    { name: "phux-desktop/bin/phux-desktop", type: "1", linkname: fx.root },
    { name: "phux-desktop/share/phux-desktop/NOTICE", type: "6" },
    { name: "phux-desktop/../../escaped" },
  ];
  for (const entry of hostile) {
    fx.pack({}, entry);
    fx.install(undefined, false);
    expect([entry.name, existsSync(fx.installed)]).toEqual([entry.name, false]);
    expect(existsSync(join(fx.root, "escaped"))).toBe(false);
  }
});

test("the lock and an unrelated directory are preserved", () => {
  const lock = join(fx.apps, ".phux-desktop-install.lock");
  mkdirSync(lock, { recursive: true });
  expect(fx.install(undefined, false).stderr).toContain("another Desktop install");
  expect(statSync(lock).isDirectory()).toBe(true);
  rmdirSync(lock);
  mkdirSync(fx.installed);
  writeFileSync(join(fx.installed, "keep"), "unrelated");
  expect(fx.install(undefined, false).stderr).toContain("unrelated");
  expect(readFileSync(join(fx.installed, "keep"), "utf8")).toBe("unrelated");
});

test("a failed publish restores the previous bundle", () => {
  fx.install();
  const original = fx.release();
  fx.pack({ sha: "b".repeat(40) });
  fx.failPublish();
  fx.install(undefined, false);
  expect(fx.release()).toBe(original);
  expect(fx.leftovers()).toEqual([]);
});

test("staging is explicitly unsupported", () => {
  const result = fx.install(["--version", VERSION, "--stage-dir", join(fx.root, "stage")], false);
  expect(result.stderr).toContain("not supported on Linux");
  expect(existsSync(fx.installed)).toBe(false);
});

test("a missing or invalid XDG_DATA_HOME uses ~/.local/share", () => {
  for (const value of [undefined, "", "relative/data"]) {
    fx.env["XDG_DATA_HOME"] = value;
    fx.install();
    const installed = join(fx.root, "home/.local/share/phux-desktop/bin/phux-desktop");
    expect([value, statSync(installed).isFile()]).toEqual([value, true]);
  }
});

test("the real packager's bundle installs with an escaped desktop entry", () => {
  fx.apps = join(fx.root, 'data "$HOME`test\\percent%f');
  fx.env["XDG_DATA_HOME"] = fx.apps;
  const binary = join(fx.root, "elf");
  const elf = new Uint8Array(20);
  elf.set([0x7f, 0x45, 0x4c, 0x46, 0x02, 0x01]);
  elf[18] = 62; // x86-64
  writeFileSync(binary, elf);
  const output = join(fx.root, "packaged");
  createLinuxBundle({
    binary,
    output,
    version: VERSION,
    sha: SHA,
    target: TARGET,
    channel: "stable",
    notices: "fixture",
  });
  const archive = join(fx.root, "asset");
  capture(["tar", "-czf", archive, "-C", output, "phux-desktop"]);
  fx.install(["--version", VERSION, "--sha256", sha256(readFileSync(archive))]);
  expect(statSync(join(fx.installed, "share/phux-desktop/LICENSE")).isFile()).toBe(true);
  const entry = readFileSync(join(fx.apps, "applications/dev.phux.desktop.desktop"), "utf8");
  // Bun's realpathSync refuses a backslash anywhere in the path, so resolve
  // the scratch root (behind /var -> /private/var on macOS) and append.
  const installed = join(realpathSync(fx.root), relative(fx.root, fx.installed));
  const quoted = join(installed, "bin/phux-desktop")
    .replace(/[\\"`$]/g, (char) => `\\${char}`)
    .replaceAll("\\", "\\\\")
    .replaceAll("%", "%%");
  expect(entry).toContain(`Exec="${quoted}"`);
  expect(entry).toContain(`Icon=${join(installed, "share/icons/").replaceAll("\\", "\\\\")}`);
});
