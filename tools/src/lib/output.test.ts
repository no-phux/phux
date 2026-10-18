// The shared presentation contract for human diagnostics, in every runtime
// that renders it: scripts/lib/output.sh under each POSIX shell, this
// package's output.ts, and scripts/lib/output.mjs while scripts still import it.
import { describe, expect, test } from "bun:test";
import { copyFileSync, existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { repoPath } from "./repo.ts";

const HELPER = repoPath("scripts/lib/output.sh");
const SHELLS = [...new Set(["/bin/sh", "/bin/bash", Bun.which("dash")])].filter(
  (shell): shell is string => shell !== null && existsSync(shell),
);
const STATUS =
  '. "$1"; phux_error "bad %s \\path"; phux_warn "careful"; phux_info "working"; phux_ok "done"; phux_hint "  try this\n  second line"; printf "data\\n"';
const PLAIN =
  "phux: error: bad %s \\path\nphux: warn: careful\nphux: info: working\nphux: ok: done\n  hint: try this\n  hint: second line\n";

function script(module: string): string {
  return `import { status, hint } from ${JSON.stringify(module)};
status("error", String.raw\`bad %s \\path\`);
status("warn", "careful");
status("info", "working");
status("ok", "done");
hint("  try this\\n  second line");
process.stdout.write("data\\n");`;
}

const COMMANDS: Record<string, string[]> = Object.fromEntries(
  SHELLS.map((shell) => [shell, [shell, "-eu", "-c", STATUS, "test", HELPER]]),
);
COMMANDS["output.ts"] = [process.execPath, "-e", script(repoPath("tools/src/lib/output.ts"))];
// The .mjs twin retires with its last importer under scripts/.
const MJS = repoPath("scripts/lib/output.mjs");
if (existsSync(MJS)) COMMANDS["output.mjs"] = [process.execPath, "-e", script(MJS)];

function environment(overrides: Record<string, string> = {}): Record<string, string> {
  const env: Record<string, string> = {};
  for (const [name, value] of Object.entries(process.env)) {
    if (value !== undefined && !["NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE"].includes(name)) {
      env[name] = value;
    }
  }
  return { ...env, TERM: "xterm-256color", ...overrides };
}

function runStatus(
  command: string[],
  env: Record<string, string> = {},
): { stdout: string; stderr: string } {
  const result = Bun.spawnSync(command, {
    cwd: repoPath("scripts"),
    env: environment(env),
    stdout: "pipe",
    stderr: "pipe",
  });
  expect(result.exitCode, result.stderr.toString()).toBe(0);
  return { stdout: result.stdout.toString(), stderr: result.stderr.toString() };
}

describe.each(Object.keys(COMMANDS))("%s", (name) => {
  const command = COMMANDS[name] ?? [];

  test("plain pipe and literal payload", () => {
    expect(runStatus(command)).toEqual({ stdout: "data\n", stderr: PLAIN });
  });

  test("color environment precedence", () => {
    const cases: [Record<string, string>, boolean][] = [
      [{}, false],
      [{ CLICOLOR_FORCE: "1" }, true],
      [{ CLICOLOR_FORCE: "0" }, false],
      [{ CLICOLOR_FORCE: "" }, false],
      [{ CLICOLOR_FORCE: "1", NO_COLOR: "1" }, false],
      [{ CLICOLOR_FORCE: "1", NO_COLOR: "" }, true],
      [{ CLICOLOR_FORCE: "1", TERM: "dumb" }, false],
      [{ CLICOLOR_FORCE: "1", CLICOLOR: "0" }, true],
      [{ CLICOLOR: "0" }, false],
    ];
    for (const [env, colored] of cases) {
      const result = runStatus(command, env);
      expect(result.stdout).toBe("data\n");
      expect([env, result.stderr.includes("\x1b[")]).toEqual([env, colored]);
      if (!colored) expect(result.stderr).toBe(PLAIN);
    }
  });

  test.each(["stdout", "stderr"])(
    "only a stderr terminal colors (%s on the PTY)",
    async (destination) => {
      const dir = mkdtempSync(join(tmpdir(), "phux-output-"));
      try {
        const other = join(dir, "other");
        // The stream not under test goes to a file; the other stays on the PTY.
        const redirect = destination === "stderr" ? '>"$PHUX_OTHER"' : '2>"$PHUX_OTHER"';
        const chunks: Uint8Array[] = [];
        const child = Bun.spawn(["/bin/sh", "-c", `exec "$@" ${redirect}`, "sh", ...command], {
          cwd: repoPath("scripts"),
          env: environment({ PHUX_OTHER: other }),
          terminal: { cols: 200, rows: 24, data: (_terminal, data) => chunks.push(data) },
        });
        expect(await child.exited).toBe(0);
        // The PTY may deliver what the child wrote after it exited.
        const text = (): string => Buffer.concat(chunks).toString().replaceAll("\r\n", "\n");
        const last = destination === "stderr" ? "second line\n" : "data\n";
        for (let waited = 0; !text().endsWith(last) && waited < 10_000; waited += 10) {
          await Bun.sleep(10);
        }
        child.terminal?.close();
        const captured = text();
        const redirected = readFileSync(other, "utf8");
        if (destination === "stderr") {
          expect(captured).toContain("\x1b[1mphux\x1b[0m: \x1b[31merror\x1b[0m:");
          expect(redirected).toBe("data\n");
        } else {
          expect(redirected).toBe(PLAIN);
          expect(captured).toBe("data\n");
        }
      } finally {
        rmSync(dir, { recursive: true, force: true });
      }
    },
  );
});

describe.each(SHELLS)("%s", (shell) => {
  test("rendering does not change caller variables or exit", () => {
    const code =
      '. "$1"; phux_label=original; phux_style=original; phux_error test; test "$phux_label/$phux_style" = original/original; exit 7';
    const result = Bun.spawnSync([shell, "-eu", "-c", code, "test", HELPER], {
      env: environment({ CLICOLOR_FORCE: "1" }),
      stdout: "pipe",
      stderr: "pipe",
    });
    expect(result.exitCode).toBe(7);
    expect(result.stdout.toString()).toBe("");
  });

  test("the served installer stands alone and errors keep stdout empty", () => {
    // Copy just the served script, proving there is no repo helper dependency.
    const dir = mkdtempSync(join(tmpdir(), "phux-install-"));
    try {
      const installer = join(dir, "install.sh");
      copyFileSync(repoPath("scripts/install.sh"), installer);
      const result = Bun.spawnSync([shell, installer, "--not-a-flag"], {
        cwd: dir,
        env: environment(),
        stdout: "pipe",
        stderr: "pipe",
      });
      const stderr = result.stderr.toString();
      expect(result.exitCode).toBe(1);
      expect(result.stdout.toString()).toBe("");
      expect(stderr).toStartWith("phux: error: ");
      expect(stderr).not.toContain("\x1b");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
