import { describe, expect, test } from "bun:test";
import {
  nativeReservationTtlMs,
  sessionDeadline,
  sessionExpiryReason,
  sessionReservationTtlMs,
  nativeUpstreamRequest,
  parseDemoMode,
  isNativeEnabled,
} from "./native-routing";

describe("native mode routing", () => {
  test("accepts only the three public modes", () => {
    expect(parseDemoMode(null)).toBe("demo");
    expect(parseDemoMode("demo")).toBe("demo");
    expect(parseDemoMode("portfolio")).toBe("portfolio");
    expect(parseDemoMode("native")).toBe("native");
    expect(parseDemoMode("NATIVE")).toBeNull();
    expect(parseDemoMode("other")).toBeNull();
    expect(parseDemoMode("native-fallback")).toBeNull();
  });

  test("defaults the kill switch on and recognizes explicit false values", () => {
    expect(isNativeEnabled(undefined)).toBe(true);
    expect(isNativeEnabled("true")).toBe(true);
    expect(isNativeEnabled("false")).toBe(false);
    expect(isNativeEnabled("OFF")).toBe(false);
    expect(isNativeEnabled("0")).toBe(false);
  });

  test("keeps the reservation through hard expiry and launch margin", () => {
    expect(nativeReservationTtlMs(300_000, 30_000)).toBe(360_000);
    expect(nativeReservationTtlMs(10_000, 90_000)).toBe(90_000);
  });

  test("reserves edge slots through hard expiry with a bounded cleanup margin", () => {
    expect(sessionReservationTtlMs(600_000)).toBe(630_000);
    expect(sessionReservationTtlMs(10_000, 5_000)).toBe(15_000);
    expect(sessionReservationTtlMs(10_000, 999_999)).toBe(70_000);
    expect(sessionReservationTtlMs(10_000, -1)).toBe(10_000);
  });

  test("selects and re-evaluates the current session deadline", () => {
    const deadlines = { idleDeadline: 120, expiresAt: 200 };
    expect(sessionDeadline(deadlines)).toBe(120);
    expect(sessionExpiryReason(119, deadlines)).toBeNull();
    expect(sessionExpiryReason(120, deadlines)).toBe("idle");
    deadlines.idleDeadline = 250;
    expect(sessionDeadline(deadlines)).toBe(200);
    expect(sessionExpiryReason(200, deadlines)).toBe("hard");
    expect(sessionExpiryReason(300, { idleDeadline: 200, expiresAt: 250 })).toBe("hard");
  });

  test("normalizes the phux endpoint and strips admission details", () => {
    const request = new Request("https://demo.example/session?mode=native&ignored=1", {
      headers: {
        Upgrade: "websocket",
        Origin: "https://phux.sh",
        "CF-Connecting-IP": "192.0.2.1",
        "X-Forwarded-For": "192.0.2.1",
        "X-Phux-Session": "sid",
        "X-Phux-Token": "secret",
      },
    });

    const upstream = nativeUpstreamRequest(request);
    expect(upstream.url).toBe("https://demo.example/");
    expect(upstream.headers.get("Upgrade")).toBe("websocket");
    expect(upstream.headers.get("Origin")).toBe("https://phux.sh");
    expect(upstream.headers.has("CF-Connecting-IP")).toBe(false);
    expect(upstream.headers.has("X-Forwarded-For")).toBe(false);
    expect(upstream.headers.has("X-Phux-Session")).toBe(false);
    expect(upstream.headers.has("X-Phux-Token")).toBe(false);
  });
});

describe("native container configuration", () => {
  test("declares the native lite container without replacing SessionDO", async () => {
    const config = await Bun.file(new URL("../wrangler.jsonc", import.meta.url)).text();
    expect(config).toContain('"name": "SESSION", "class_name": "SessionDO"');
    expect(config).toContain('"name": "RATE_LIMIT", "class_name": "RateLimitDO"');
    expect(config).toContain('"name": "PHUX_SESSION"');
    expect(config).toContain('"class_name": "PhuxSessionContainer"');
    expect(config).toContain('"instance_type": "lite"');
    expect(config).toContain('"max_instances": 30');
    expect(config).toContain('"tag": "v2"');
    expect(config).toContain('"tag": "v3"');
    expect(config).toContain('"NATIVE_HARD_MAX_MS": "300000"');
    expect(config).toContain('"NATIVE_CONCURRENCY_CAP": "25"');
    expect(config).toContain('"NATIVE_ENABLED": "true"');
    expect(config).toContain('"NATIVE_STARTUP_TIMEOUT_MS": "8000"');
    expect(config).toContain('"NATIVE_PER_IP_CAP": "1"');
    expect(config).toContain('"NATIVE_RATE_LIMIT_PER_MIN": "2"');
    expect(config).toContain('"NATIVE_PER_ACCOUNT_CAP": "1"');
    expect(config).toContain('"NATIVE_LAUNCHES_PER_HOUR": "6"');
    expect(config).toContain('"NATIVE_DAILY_MS": "1800000"');
  });

  test("runs the image as the unprivileged guest user", async () => {
    const dockerfile = await Bun.file(
      new URL("../Dockerfile", import.meta.url),
    ).text();
    expect(dockerfile).toContain("USER 10001:10001");
    expect(dockerfile).toContain("EXPOSE 8080 8082");
    expect(dockerfile).toContain("RUSTFLAGS=\"-C target-cpu=x86-64\"");
    expect(dockerfile).toContain('target == "x86_64-unknown-linux-gnu"');
    expect(dockerfile).toContain("/opt/zig/lib/std/Random.zig");
    expect(dockerfile).toContain("max - 1);/'");
    expect(dockerfile).toContain("bun-linux-x64-baseline");
    expect(dockerfile).not.toContain("setpriv --reuid");
  });

  test("provides a shell toolchain without restoring package management", async () => {
    const dockerfile = await Bun.file(
      new URL("../Dockerfile", import.meta.url),
    ).text();

    for (const tool of [
      "build-essential",
      "curl",
      "fd-find",
      "file",
      "git",
      "jq",
      "nodejs",
      "python3",
      "python3-pip",
      "ripgrep",
      "unzip",
    ]) {
      expect(dockerfile).toMatch(new RegExp(`\\s${tool.replace("+", "\\+")}=`));
    }
    expect(dockerfile).toContain("ln -sf /usr/bin/fdfind /usr/local/bin/fd");
    expect(dockerfile).toContain("rm -rf /var/lib/apt/lists/*");
    expect(dockerfile).toContain("/usr/lib/apt /var/lib/dpkg");
    expect(dockerfile).toContain("rm -f /usr/bin/apt* /usr/bin/dpkg*");
    expect(dockerfile).not.toMatch(/\s(?:openssh-server|systemd|cron)=/);
  });

  test("keeps native shells offline, credential-free, and ephemeral", async () => {
    const [nativeSession, entrypoint, bashrc] = await Promise.all([
      Bun.file(new URL("./native-session.ts", import.meta.url)).text(),
      Bun.file(new URL("../container/entrypoint.sh", import.meta.url)).text(),
      Bun.file(new URL("../container/bashrc", import.meta.url)).text(),
    ]);

    expect(nativeSession).toContain("enableInternet = false");
    expect(entrypoint).toContain("/usr/bin/env -i");
    expect(entrypoint).toContain("HOME=/tmp");
    expect(entrypoint).toContain("XDG_CACHE_HOME=/tmp/.cache");
    expect(entrypoint).toContain("/usr/local/bin/phux server --session default");
    expect(bashrc).toContain("HISTFILE=/dev/null");
    expect(bashrc).toContain("unset GITHUB_TOKEN GH_TOKEN SSH_AUTH_SOCK");
  });
});
