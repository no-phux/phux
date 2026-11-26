// Offline contract tests: source-only is honest, missing listed assets are not.
import { afterAll, beforeEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { Failure } from "../lib/command.ts";
import { repoPath } from "../lib/repo.ts";
import { str, table } from "../lib/value.ts";
import { checkGhosttyPin, type Fetch, zigCompatible } from "./ghostty-pin.ts";

const REV = "0123456789abcdef0123456789abcdef01234567";
const DIGEST = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const ZIG =
  str(table(JSON.parse(readFileSync(repoPath(".config/zig-toolchain.json"), "utf8")))["version"]) ??
  "";
const MINOR = ZIG.replace(/\.[^.]*$/, "");

const root = mkdtempSync(join(tmpdir(), "phux-pin-test-"));
afterAll(() => rmSync(root, { recursive: true, force: true }));

function put(path: string, content: string): void {
  mkdirSync(dirname(join(root, path)), { recursive: true });
  writeFileSync(join(root, path), content);
}

/** What the fake network serves; each test starts from a passing pin. */
interface World {
  buildRs: string;
  downloadRs: string;
  zon: string;
  offline: boolean;
  sourceHttp: number;
  assetHttp: number;
  assets: string[] | undefined;
}
let world: World;

const archives = (
  body: string,
) => `const PREBUILT_BASE_URL: &str = "https://github.com/example/bindings/releases/download";
const PREBUILT_OPTIMIZE: &str = "fast";
const PREBUILT_CPU: &str = "baseline";
const PREBUILT_ARCHIVES: &[(&str, &str)] = &[${body}];
`;

beforeEach(() => {
  put(
    "Cargo.lock",
    `[[package]]\nname = "libghostty-vt-sys"\nsource = "git+https://github.com/example/bindings.git?rev=${REV}#${REV}"\n`,
  );
  put(
    "clients/phux-vt-web/scripts/build-vt-wasm.sh",
    `revision=${REV}\narchive_sha256=${DIGEST}\n# https://codeload.github.com/example/engine/tar.gz/$revision\n`,
  );
  put(".config/zig-toolchain.json", JSON.stringify({ version: ZIG }));
  put("nix/package.nix", `  ghosttyRev = "${REV}";\n`);
  world = {
    buildRs: `const GHOSTTY_REPO: &str = "https://github.com/example/engine.git";\nconst GHOSTTY_COMMIT: &str = "${REV}";\n`,
    downloadRs: archives(""),
    zon: `.minimum_zig_version = "${ZIG}",\n`,
    offline: false,
    sourceHttp: 200,
    assetHttp: 200,
    assets: undefined,
  };
});

const fakeFetch: Fetch = (url) => {
  if (world.offline) return Promise.reject(new TypeError("offline"));
  const reply = (body: string | null, status = 200) =>
    Promise.resolve(new Response(body, { status }));
  if (url.endsWith("/build.rs")) return reply(world.buildRs);
  if (url.endsWith("/download.rs")) return reply(world.downloadRs);
  if (url.endsWith("/build.zig.zon")) return reply(world.zon, world.sourceHttp);
  if (/\/libghostty-vt-[^/]*\.tar\.gz$/.test(url)) return reply(null, world.assetHttp);
  if (url.startsWith("https://api.github.com/") && world.assets !== undefined) {
    return reply(JSON.stringify({ assets: world.assets.map((name) => ({ name })) }));
  }
  return Promise.reject(new Error(`unexpected request ${url}`));
};

/** The check's log, or the failure message it threw. */
async function run(): Promise<{ failed: boolean; output: string }> {
  const lines: string[] = [];
  try {
    await checkGhosttyPin({ root, fetch: fakeFetch, log: (_, message) => lines.push(message) });
    return { failed: false, output: lines.join("\n") };
  } catch (error) {
    if (!(error instanceof Failure)) throw error;
    return { failed: true, output: [...lines, error.message].join("\n") };
  }
}

async function passes(message: string): Promise<void> {
  const result = await run();
  expect(result).toMatchObject({ failed: false });
  expect(result.output).toContain(message);
}

async function fails(message: string): Promise<void> {
  const result = await run();
  expect(result).toMatchObject({ failed: true });
  expect(result.output).toContain(message);
}

test("a source-only pin passes honestly", () => passes("0 prebuilt, 4 source-only targets"));

test("a binding revision that differs from its resolved commit fails", async () => {
  put(
    "Cargo.lock",
    `[[package]]\nname = "libghostty-vt-sys"\nsource = "git+https://github.com/example/bindings.git?rev=${"f".repeat(40)}#${REV}"\n`,
  );
  await fails("binding revision does not match");
});

test("a browser engine on another revision fails", async () => {
  put(
    "clients/phux-vt-web/scripts/build-vt-wasm.sh",
    `revision=bad\narchive_sha256=${DIGEST}\n# https://codeload.github.com/example/engine/tar.gz/$revision\n`,
  );
  await fails("differs from browser engine");
});

test("a Nix package on another engine revision fails", async () => {
  put("nix/package.nix", `  ghosttyRev = "${"f".repeat(40)}";\n`);
  await fails("differs from the Nix package's ffff");
});

test("an unreachable network skips instead of claiming verification", async () => {
  world.offline = true;
  await passes("skipped");
});

test("an engine source the host does not serve fails", async () => {
  world.sourceHttp = 404;
  await fails("is not served (HTTP 404)");
});

test("the engine's minimum Zig must be the pinned minor, at or below the pin", async () => {
  world.zon = '.minimum_zig_version = "0.0.0",\n';
  await fails("engine requires Zig 0.0.0");
  world.zon = `.minimum_zig_version = "${MINOR}.0",\n`;
  await passes("0 prebuilt, 4 source-only targets");
  world.zon = `.minimum_zig_version = "${MINOR}.99",\n`;
  await fails(`engine requires Zig ${MINOR}.99`);
});

test("a branch name is not an immutable engine revision", async () => {
  world.buildRs = world.buildRs.replace(REV, "main");
  await fails("not an immutable full commit");
});

test("a missing coverage table fails", async () => {
  world.downloadRs = world.downloadRs.replace(/^const PREBUILT_ARCHIVES:.*\n/m, "");
  await fails("prebuilt coverage table is missing");
});

test("a declared prebuilt needs a full checksum", async () => {
  world.downloadRs = archives('("aarch64-apple-darwin", "bad")');
  await fails("malformed prebuilt checksum");
});

test("a declared prebuilt counts only when its release serves it", async () => {
  world.downloadRs = archives(`("aarch64-apple-darwin", "${DIGEST}")`);
  await passes("1 prebuilt, 3 source-only targets");
  world.assetHttp = 404;
  await fails("is not served");
});

test("rustfmt may split a table entry over lines", async () => {
  world.downloadRs = archives(
    `\n    (\n        "aarch64-apple-darwin",\n        "${DIGEST}",\n    ),\n`,
  );
  await passes("1 prebuilt, 3 source-only targets");
});

test("a 5xx asset falls back to the release listing", async () => {
  world.downloadRs = archives(`("aarch64-apple-darwin", "${DIGEST}")`);
  world.assetHttp = 502;
  world.assets = ["libghostty-vt-aarch64-apple-darwin.tar.gz"];
  await passes("1 prebuilt, 3 source-only targets");
  world.assets = [];
  await fails("the release does not list it");
  world.assets = undefined;
  await passes("skipped");
});

test("zig compatibility is a same-minor minimum", () => {
  expect(zigCompatible("0.17.0", "0.17.1")).toBe(true);
  expect(zigCompatible("0.17.1", "0.17.1")).toBe(true);
  expect(zigCompatible("0.17.2", "0.17.1")).toBe(false);
  expect(zigCompatible("0.16.9", "0.17.1")).toBe(false);
  expect(zigCompatible("", "0.17.1")).toBe(false);
});
