// Keep one current build cache per lane/release prefix.
//
// A key from the current UTC day is never deleted. When one exists, older
// keys for that prefix go. When it does not, the newest older key stays so
// a restore still has a source. Only caches on main participate. Call this
// after job post-step uploads: eligibility or a successful build does not
// prove that an export was nonempty or that GitHub stored its replacement.
import { gh, ghLines } from "../lib/github.ts";
import { str, table } from "../lib/value.ts";

const MAIN = "refs/heads/main";

/**
 * One series each. ios-native is the Rune simulator leg's rust-cache key;
 * web-vt and web-client are the browser lane's rust-cache keys.
 */
export const SWEEP = [
  "linux-arm64-mbx-phux-check-",
  "linux-arm64-mbx-phux-test-",
  "v0-rust-phux-uniffi-",
  "v0-rust-phux-web-vt-",
  "v0-rust-phux-web-client-",
  "linux-x64-mbx-phux-icount-",
  "v0-rust-ffi-android-Linux-x64-",
  "v0-rust-release-deps-Linux-x64-",
  "v0-rust-release-deps-Linux-arm64-",
  "v0-rust-release-deps-Darwin-arm64-",
  "v0-rust-mini-v1-desktop-release-",
  "mini-v1-ffi-xcframework-zig-",
  "v0-rust-windows-desktop-wsl-server-Linux-x64-",
  "v0-rust-windows-desktop-msvc-Windows_NT-x64-",
  "v0-rust-desktop-macOS-Darwin-",
  "v0-rust-desktop-macOS-26-Darwin-",
  "v0-rust-desktop-Linux-Linux-",
  "v0-rust-rune-macos-arm64-Darwin-",
  "v0-rust-rune-linux-x86_64-Linux-",
  "v0-rust-rune-linux-aarch64-Linux-",
  "v0-rust-rune-ios-native-",
] as const;

export interface CacheEntry {
  id: string;
  key: string;
  created: string;
}

/** The caches GitHub holds; a test stands in a fixture. */
export interface CacheStore {
  /** Main's caches whose key starts with `prefix`, newest first. */
  list(prefix: string): CacheEntry[];
  remove(id: string): void;
}

/** The listing query: main's caches for one prefix, newest first. */
export function listEndpoint(repository: string, prefix: string): string {
  return `repos/${repository}/actions/caches?key=${prefix}&ref=${MAIN}&sort=created_at&direction=desc&per_page=100`;
}

/** The repository's caches through `gh`. */
export function githubStore(repository: string): CacheStore {
  return {
    list: (prefix) =>
      ghLines([
        "api",
        "--paginate",
        listEndpoint(repository, prefix),
        "--jq",
        `.actions_caches[] | select(.ref == "${MAIN}") | {id: (.id | tostring), key, created_at}`,
      ]).map((line) => {
        const row = table(line);
        return {
          id: str(row["id"]) ?? "",
          key: str(row["key"]) ?? "",
          created: str(row["created_at"]) ?? "",
        };
      }),
    remove(id) {
      // A failure (404: another main run already pruned it) is not fatal.
      try {
        gh(["api", "-X", "DELETE", `repos/${repository}/actions/caches/${id}`, "--silent"]);
      } catch {
        // Keep sweeping.
      }
      Bun.sleepSync(250);
    },
  };
}

/** A key stamped with today's UTC date, or a cache created today. */
export function isCurrentDay(entry: CacheEntry, now: Date): boolean {
  const iso = now.toISOString().slice(0, 10);
  return entry.key.endsWith(`-${iso.replaceAll("-", "")}`) || entry.created.startsWith(iso);
}

/** The caches to delete from one prefix's newest-first listing. */
export function superseded(entries: readonly CacheEntry[], now: Date): CacheEntry[] {
  const current = entries.filter((entry) => entry.id !== "" && isCurrentDay(entry, now));
  const older = entries.filter((entry) => entry.id !== "" && !isCurrentDay(entry, now));
  // With no current-day replacement, the newest older cache stays as the restore source.
  return current.length > 0 ? older : older.slice(1);
}

/** The log line for each decision, in listing order. */
function describeEntry(entry: CacheEntry, now: Date, doomed: Set<string>, dry: boolean): string {
  if (isCurrentDay(entry, now)) return `keeping current-day ${entry.key}`;
  if (!doomed.has(entry.id)) return `keeping newest restore ${entry.key}`;
  return `${dry ? "would delete" : "deleting"} ${entry.key} (${entry.id})`;
}

/** Sweep every prefix. A failed listing throws before that prefix deletes anything. */
export function prune(
  store: CacheStore,
  now: Date,
  dry: boolean,
  prefixes: readonly string[] = SWEEP,
): void {
  for (const prefix of prefixes) {
    const entries = store.list(prefix).filter((entry) => entry.id !== "");
    if (entries.length === 0) {
      console.log(`no caches for ${prefix}`);
      continue;
    }
    const doomed = new Set(superseded(entries, now).map((entry) => entry.id));
    for (const entry of entries) {
      console.log(describeEntry(entry, now, doomed, dry));
      if (doomed.has(entry.id) && !dry) store.remove(entry.id);
    }
  }
}
