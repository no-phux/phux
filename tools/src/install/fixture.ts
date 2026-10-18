// Shared scaffolding for the scripts/install.sh --desktop e2e tests: a scratch
// root whose files a fake curl serves, and the installer run against it.
import { expect } from "bun:test";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";

export const VERSION = "1.2.3";
const SHELL = Bun.which("dash") ?? "/bin/sh";
const FAKE_CURL = String.raw`#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in -o) out="$2"; shift ;; https:*) url="$1" ;; esac
  shift
done
printf '%s\n' "$url" >> "$FIXTURE/calls"
case "$url" in
  */channels/*.json) file=manifest.json ;;
  *SHA256SUMS | *.sha256) file=SHA256SUMS ;;
  *.minisig) file=signature ;;
  */phux-desktop-*) file=asset ;;
  *.tar.gz) file=cli.tar.gz ;;
  *) exit 22 ;;
esac
[ -f "$FIXTURE/$file" ] || exit 22
cp "$FIXTURE/$file" "$out"
`;

export function sha256(data: Uint8Array | string): string {
  return new Bun.CryptoHasher("sha256").update(data).digest("hex");
}

/** JSON as release tooling writes it; install.sh reads it line by line. */
export function json(value: unknown): string {
  return `${JSON.stringify(value, null, 2)}\n`;
}

export interface Result {
  readonly stdout: string;
  readonly stderr: string;
}

export abstract class InstallFixture {
  readonly root = mkdtempSync(join(tmpdir(), "phux-desktop-install-"));
  readonly bin = join(this.root, "bin");
  readonly launchers = join(this.root, "launchers");
  apps = join(this.root, "apps with spaces");
  digest = "";
  readonly env: Record<string, string | undefined> = {
    HOME: join(this.root, "home"),
    FIXTURE: this.root,
    PATH: `${this.bin}:/usr/bin:/bin:/usr/sbin:/sbin`,
  };

  constructor(
    /** The installed bundle's name under `apps`. */
    readonly bundle: string,
    /** The release asset name the served archive is published as. */
    public asset: string,
  ) {
    mkdirSync(this.bin);
    mkdirSync(this.launchers);
    // A CLI is already installed unless a test removes it.
    symlinkSync("/usr/bin/true", join(this.launchers, "phux"));
    this.script("curl", FAKE_CURL);
  }

  /** The installer arguments that pick the platform and its destination. */
  abstract platform(): string[];

  get installed(): string {
    return join(this.apps, this.bundle);
  }

  script(name: string, text: string): void {
    writeFileSync(join(this.bin, name), text);
    chmodSync(join(this.bin, name), 0o755);
  }

  /** Serve `data` as the desktop asset with its SHA256SUMS line. */
  serve(data: Uint8Array): void {
    writeFileSync(join(this.root, "asset"), data);
    this.digest = sha256(data);
    writeFileSync(join(this.root, "SHA256SUMS"), `${this.digest}  ${this.asset}\n`);
  }

  /** Make publishing the new bundle fail, as a full disk would. */
  failPublish(): void {
    const target = `*/.phux-desktop-install.*/${this.bundle}`;
    this.script("mv", `#!/bin/sh\ncase "$1" in ${target}) exit 1 ;; esac\nexec /bin/mv "$@"\n`);
  }

  run(args: readonly string[], ok = true): Result {
    const installer = [SHELL, join(ROOT, "scripts/install.sh"), "--desktop"];
    const result = Bun.spawnSync([...installer, ...args], {
      env: this.env,
      stdout: "pipe",
      stderr: "pipe",
    });
    const [stdout, stderr] = [result.stdout.toString(), result.stderr.toString()];
    expect(result.exitCode === 0, stdout + stderr).toBe(ok);
    return { stdout, stderr };
  }

  install(args = ["--version", VERSION], ok = true): Result {
    return this.run([...this.platform(), "--install-dir", this.launchers, ...args], ok);
  }

  /** Installer scratch and lock entries left beside the bundle. */
  leftovers(): string[] {
    return existsSync(this.apps)
      ? readdirSync(this.apps).filter((name) => name.startsWith(".phux-desktop"))
      : [];
  }

  get calls(): string[] {
    const path = join(this.root, "calls");
    return existsSync(path) ? readFileSync(path, "utf8").split("\n").filter(Boolean) : [];
  }

  cleanup(): void {
    rmSync(this.root, { recursive: true, force: true });
  }
}
