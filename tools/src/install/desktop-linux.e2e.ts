// scripts/install.sh --desktop on Linux: portable installer transactions with
// real archives and a fake network. Runs on any POSIX host.
//
// Run explicitly (`bun test ./tools/src/install/desktop-linux.e2e.ts`); it is
// not part of the default `bun test` discovery.
import { beforeEach, describe, expect, test } from "bun:test";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmdirSync,
  statSync,
  symlinkSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join, relative } from "node:path";
import { ROOT } from "../lib/repo.ts";

const SHELL = Bun.which("dash") ?? "/bin/sh";
const FAKE_CURL = String.raw`#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in -o) out="$2"; shift ;; https:*) url="$1" ;; esac
  shift
done
printf '%s\n' "$url" >> "$FIXTURE/calls"
case "$url" in
  */channels/*.json) file=manifest.json ;;
  *SHA256SUMS | *.sha256) file=checksum ;;
  *.minisig) file=signature ;;
  *.tar.gz) file=archive.tar.gz ;;
  *) exit 22 ;;
esac
cp "$FIXTURE/$file" "$out"
`;

// --- a minimal ustar writer: the special entries need exact control ------

type EntryType = "file" | "dir" | "symlink" | "hardlink" | "fifo";
const TYPEFLAG: Record<EntryType, string> = {
  file: "0",
  hardlink: "1",
  symlink: "2",
  fifo: "6",
  dir: "5",
};

interface Entry {
  readonly name: string;
  readonly type?: EntryType;
  readonly mode?: number;
  readonly content?: Uint8Array;
  readonly linkname?: string;
}

function header(entry: Entry): Uint8Array {
  const block = new Uint8Array(512);
  const put = (offset: number, length: number, text: string) =>
    block.set(new TextEncoder().encode(text).subarray(0, length), offset);
  const octal = (offset: number, length: number, value: number) =>
    put(offset, length, `${value.toString(8).padStart(length - 1, "0")}\0`);
  put(0, 100, entry.name);
  octal(100, 8, entry.mode ?? 0o644);
  octal(108, 8, 0);
  octal(116, 8, 0);
  octal(124, 12, entry.content?.length ?? 0);
  octal(136, 12, 0);
  put(148, 8, " ".repeat(8));
  put(156, 1, TYPEFLAG[entry.type ?? "file"]);
  put(157, 100, entry.linkname ?? "");
  put(257, 8, "ustar\u000000");
  const sum = block.reduce((total, byte) => total + byte, 0);
  put(148, 8, `${sum.toString(8).padStart(6, "0")}\0 `);
  return block;
}

function tarGz(entries: readonly Entry[]): Uint8Array {
  const blocks: Uint8Array[] = [];
  for (const entry of entries) {
    blocks.push(header(entry));
    const content = entry.content ?? new Uint8Array();
    const padded = new Uint8Array(Math.ceil(content.length / 512) * 512);
    padded.set(content);
    blocks.push(padded);
  }
  blocks.push(new Uint8Array(1024));
  const tar = new Uint8Array(blocks.reduce((total, block) => total + block.length, 0));
  let offset = 0;
  for (const block of blocks) {
    tar.set(block, offset);
    offset += block.length;
  }
  return Bun.gzipSync(tar);
}

// --- fixture -------------------------------------------------------------

const TARGET = "x86_64-unknown-linux-gnu";
const encode = (text: string) => new TextEncoder().encode(text);

class Fixture {
  readonly root = mkdtempSync(join(tmpdir(), "phux-linux-installer-"));
  readonly bin = join(this.root, "bin");
  apps = join(this.root, "data with spaces");
  readonly launchers = join(this.root, "launchers");
  env: Record<string, string>;
  metadata: Record<string, string> = {
    identifier: "dev.phux.desktop",
    version: "1.2.3",
    sha: "a".repeat(40),
    channel: "stable",
    target: TARGET,
  };
  asset = "phux-desktop-1.2.3-linux-x64.tar.gz";
  digest = "";

  constructor() {
    mkdirSync(this.bin);
    mkdirSync(this.launchers);
    symlinkSync("/usr/bin/true", join(this.launchers, "phux"));
    this.script("curl", FAKE_CURL);
    this.env = {
      PATH: `${this.bin}:/usr/bin:/bin`,
      HOME: join(this.root, "home"),
      FIXTURE: this.root,
      XDG_DATA_HOME: this.apps,
    };
    this.pack();
  }

  get installed(): string {
    return join(this.apps, "phux-desktop");
  }

  script(name: string, text: string): void {
    writeFileSync(join(this.bin, name), text);
    chmodSync(join(this.bin, name), 0o755);
  }

  pack(extra?: Entry, executableIsDir = false): void {
    const executable: Entry = executableIsDir
      ? { name: "phux-desktop/bin/phux-desktop", type: "dir", mode: 0o755 }
      : {
          name: "phux-desktop/bin/phux-desktop",
          mode: 0o755,
          content: encode('#!/bin/sh\nprintf "linux:%s\\n" "$*"\n'),
        };
    const release: Entry = {
      name: "phux-desktop/share/phux-desktop/release.json",
      content: encode(`${JSON.stringify(this.metadata, null, 2)}\n`),
    };
    this.writeArchive(tarGz([executable, release, ...(extra ? [extra] : [])]));
  }

  writeArchive(archive: Uint8Array): void {
    writeFileSync(join(this.root, "archive.tar.gz"), archive);
    this.digest = createHash("sha256").update(archive).digest("hex");
    writeFileSync(join(this.root, "checksum"), `${this.digest}  ${this.asset}\n`);
  }

  install(args: string[] = ["--version", "1.2.3"], ok = true): { stdout: string; stderr: string } {
    const result = Bun.spawnSync(
      [
        SHELL,
        join(ROOT, "scripts/install.sh"),
        "--desktop",
        "--os",
        "linux",
        "--arch",
        "x86_64",
        "--install-dir",
        this.launchers,
        ...args,
      ],
      { env: this.env, stdout: "pipe", stderr: "pipe" },
    );
    const output = { stdout: result.stdout.toString(), stderr: result.stderr.toString() };
    expect([result.exitCode === 0, output.stdout + output.stderr]).toEqual([
      ok,
      output.stdout + output.stderr,
    ]);
    return output;
  }

  releaseJson(): string {
    return readFileSync(join(this.installed, "share/phux-desktop/release.json"), "utf8");
  }

  staging(): string[] {
    return existsSync(this.apps)
      ? readdirSync(this.apps).filter((name) => name.startsWith(".phux-desktop"))
      : [];
  }
}

let fx: Fixture;
beforeEach(() => {
  fx = new Fixture();
});

describe("linux desktop installer", () => {
  test("install, update, launcher and the default XDG destination", () => {
    fx.install();
    const launched = Bun.spawnSync([join(fx.launchers, "phux-desktop"), "argument with spaces"], {
      stdout: "pipe",
    });
    expect(launched.stdout.toString()).toBe("linux:argument with spaces\n");
    fx.metadata["sha"] = "b".repeat(40);
    fx.pack();
    fx.install();
    expect(JSON.parse(fx.releaseJson())).toMatchObject({ sha: "b".repeat(40) });
    expect(fx.staging()).toEqual([]);
  });

  test("both architectures and next names", () => {
    Object.assign(fx.metadata, { channel: "next", target: "aarch64-unknown-linux-gnu" });
    fx.asset = `phux-desktop-next.${"a".repeat(40)}-linux-arm64.tar.gz`;
    fx.pack();
    const result = fx.install([
      "--arch",
      "arm64",
      "--channel",
      "next",
      "--revision",
      "a".repeat(40),
    ]);
    expect(result.stderr).toContain("installed Phux");
    expect(readFileSync(join(fx.root, "calls"), "utf8")).toContain(fx.asset);
  });

  test("manifest resolution", () => {
    const manifest = {
      schema_version: 1,
      channel: "stable",
      products: {
        desktop: {
          version: "1.2.3",
          sha: "a".repeat(40),
          protocol: "0.9",
          artifacts: { [TARGET]: { sha256: fx.digest } },
        },
      },
    };
    writeFileSync(join(fx.root, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
    fx.install(["--channel", "stable"]);
    expect(statSync(fx.installed).isDirectory()).toBe(true);
  });

  test("a checksum or required signature refuses without replacing", () => {
    fx.install();
    const before = fx.releaseJson();
    expect(fx.install(["--version", "1.2.3", "--sha256", "0".repeat(64)], false).stderr).toContain(
      "checksum mismatch",
    );
    fx.env["PHUX_REQUIRE_SIGNATURE"] = "1";
    expect(fx.install(undefined, false).stderr).toMatch(/minisign|signature/);
    expect(fx.releaseJson()).toBe(before);
  });

  test("an invalid signature is refused even with an installed CLI", () => {
    fx.install();
    const before = fx.releaseJson();
    fx.script("minisign", "#!/bin/sh\nexit 1\n");
    writeFileSync(join(fx.root, "signature"), "foreign signature");
    fx.env["PHUX_REQUIRE_SIGNATURE"] = "1";
    fx.install(undefined, false);
    expect(fx.releaseJson()).toBe(before);
  });

  test("a directory is not a desktop executable", () => {
    fx.install();
    const executable = join(fx.installed, "bin/phux-desktop");
    const before = readFileSync(executable, "utf8");
    fx.pack(undefined, true);
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
    const changes: [string, string][] = [
      ["identifier", "dev.example.other"],
      ["version", "9.9.9"],
      ["target", "aarch64-unknown-linux-gnu"],
      ["channel", "next"],
    ];
    for (const [field, value] of changes) {
      const original = fx.metadata[field] ?? "";
      fx.metadata[field] = value;
      fx.pack();
      fx.install(undefined, false);
      expect([field, existsSync(fx.installed)]).toEqual([field, false]);
      fx.metadata[field] = original;
    }
  });

  test("links, traversal and special files are refused before extraction", () => {
    const hostile: Entry[] = [
      { name: "phux-desktop/share", type: "symlink", linkname: fx.root },
      { name: "phux-desktop/bin/phux-desktop", type: "hardlink", linkname: fx.root },
      { name: "phux-desktop/share/phux-desktop/NOTICE", type: "fifo" },
      { name: "phux-desktop/../../escaped" },
    ];
    for (const entry of hostile) {
      fx.pack(entry);
      fx.install(undefined, false);
      expect([entry.name, existsSync(fx.installed)]).toEqual([entry.name, false]);
      expect(existsSync(join(fx.root, "escaped"))).toBe(false);
    }
  });

  test("the lock and an unrelated directory are preserved", () => {
    mkdirSync(fx.apps);
    const lock = join(fx.apps, ".phux-desktop-install.lock");
    mkdirSync(lock);
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
    const original = fx.releaseJson();
    fx.metadata["sha"] = "b".repeat(40);
    fx.pack();
    fx.script(
      "mv",
      '#!/bin/sh\ncase "$1" in */.phux-desktop-install.*/phux-desktop) exit 1 ;; esac\nexec /bin/mv "$@"\n',
    );
    fx.install(undefined, false);
    expect(fx.releaseJson()).toBe(original);
    expect(fx.staging()).toEqual([]);
  });

  test("staging is explicitly unsupported", () => {
    const result = fx.install(["--version", "1.2.3", "--stage-dir", join(fx.root, "stage")], false);
    expect(result.stderr).toContain("not supported on Linux");
    expect(existsSync(fx.installed)).toBe(false);
  });

  test("without XDG_DATA_HOME it uses ~/.local/share", () => {
    delete fx.env["XDG_DATA_HOME"];
    fx.install();
    expect(
      statSync(join(fx.root, "home/.local/share/phux-desktop/bin/phux-desktop")).isFile(),
    ).toBe(true);
  });

  test("an invalid XDG_DATA_HOME uses ~/.local/share", () => {
    for (const value of ["", "relative/data"]) {
      fx.env["XDG_DATA_HOME"] = value;
      fx.install();
      const installed = join(fx.root, "home/.local/share/phux-desktop/bin/phux-desktop");
      expect([value, statSync(installed).isFile()]).toEqual([value, true]);
    }
  });

  const packagedBundleIsAccepted = async () => {
    const binary = join(fx.root, "elf");
    const elf = new Uint8Array(20);
    elf.set([0x7f, 0x45, 0x4c, 0x46, 0x02, 0x01]);
    elf[18] = 62;
    writeFileSync(binary, elf);
    const { createLinuxBundle } = await import("../../../clients/desktop/scripts/package-linux.ts");
    createLinuxBundle({
      binary,
      output: join(fx.root, "packaged"),
      version: "1.2.3",
      sha: "a".repeat(40),
      target: TARGET,
      channel: "stable",
      notices: "fixture",
    });
    const archive = join(fx.root, "archive.tar.gz");
    const tar = Bun.spawnSync([
      "tar",
      "-czf",
      archive,
      "-C",
      join(fx.root, "packaged"),
      "phux-desktop",
    ]);
    expect(tar.exitCode).toBe(0);
    const digest = createHash("sha256").update(readFileSync(archive)).digest("hex");
    fx.install(["--version", "1.2.3", "--sha256", digest]);
    expect(statSync(join(fx.installed, "share/phux-desktop/LICENSE")).isFile()).toBe(true);
    const entry = readFileSync(join(fx.apps, "applications/dev.phux.desktop.desktop"), "utf8");
    // Bun's realpathSync refuses a backslash anywhere in the path, so resolve
    // the scratch root (behind /var -> /private/var on macOS) and append.
    const installed = join(realpathSync(fx.root), relative(fx.root, fx.installed));
    const executable = join(installed, "bin/phux-desktop");
    const quoted = executable
      .replace(/[\\"`$]/g, (char) => `\\${char}`)
      .replaceAll("\\", "\\\\")
      .replaceAll("%", "%%");
    expect(entry).toContain(`Exec="${quoted}"`);
    const icon = join(installed, "share/icons/").replaceAll("\\", "\\\\");
    expect(entry).toContain(`Icon=${icon}`);
  };

  test("the real packager's bundle is accepted", packagedBundleIsAccepted);

  test("the desktop entry escapes Exec field codes and shell characters", async () => {
    fx.apps = join(fx.root, 'data "$HOME`test\\percent%f');
    fx.env["XDG_DATA_HOME"] = fx.apps;
    await packagedBundleIsAccepted();
  });
});
