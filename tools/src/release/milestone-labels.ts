// Verify every non-closed bead carries exactly one release-milestone label
// (`rc-1.0` or `post-1.0`), by querying the LIVE Dolt store through `bd`.
//
// WHY THIS EXISTS: phux-i7vu labelled all 21 then-unlabelled open beads and
// wrote down the criterion (rc-1.0 if it touches or falsifies a surface
// ADR-0071 point 1 freezes; post-1.0 otherwise). Nothing kept that true. The
// tracker's own milestone view -- "what is left for 1.0" -- is a label query,
// so an unlabelled bead is not merely untriaged, it is INVISIBLE to the
// query. A milestone view that silently undercounts is worse than no view,
// because people plan against it.
//
// WHY IT DOES NOT READ .beads/issues.jsonl: that file is a passive export,
// and in this repo it is also DELIBERATELY SCRUBBED -- commit 51a33f4c moved
// private beads out of the public repo and slimmed the tracked JSONL, and
// `export.auto` does not make it a mirror. Measured on 2026-08-15: the export
// held 785 records while the live store held 929. A check reading the export
// would therefore be wrong in BOTH directions. A gate that can be wrong in
// both directions trains people to ignore it, so this one reads the store or
// reads nothing, and says which in its own output.
//
// WHY IT IS NOT IN `just ci`: CI checks out the repo and gets no Dolt store
// at all (`.beads/dolt/` and `.beads/embeddeddolt/` are gitignored), and
// `bd list` exits 1 with "no beads database found". There is nothing
// authoritative for CI to query, so this is a local, advisory gate -- `just
// milestone-check`, run at session close.
//
// WHY "NON-CLOSED" AND NOT A STATUS ALLOWLIST: `bd list --status
// open,in_progress` has a hole. The store also holds `deferred` and `routed`
// beads -- both live work, and `routed` is not even in `bd list --help`'s
// documented status vocabulary. Any hardcoded allowlist silently drops
// whatever bd adds next. So this enumerates every bead and treats "not
// closed" as in scope. Relatedly: repeating `-s/--status` SILENTLY
// OVERWRITES rather than unioning, its own way to undercount.
//
// FAILURE POLICY: an unlabelled or double-labelled non-closed bead is a hard
// failure (exit 1). No `bd` on PATH, or no local store, is SKIPPED and exit 0
// -- but it prints why, and it never prints a verdict about labels it could
// not check.
import { readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import { str, strings, table, type Table } from "../lib/value.ts";

const PREFIX = "milestone-labels";
const MILESTONES = ["rc-1.0", "post-1.0"];

class BdError extends Error {}

function bd(args: string[], root: string): string {
  let result;
  try {
    result = Bun.spawnSync(["bd", ...args], { cwd: root, stdout: "pipe", stderr: "pipe" });
  } catch (error) {
    throw new BdError(error instanceof Error ? error.message : String(error));
  }
  if (result.exitCode !== 0) throw new BdError(result.stderr.toString());
  return result.stdout.toString();
}

function skip(reason: string, detail?: string): number {
  process.stdout.write(
    `${PREFIX}: SKIPPED (${reason})\n` +
      `${PREFIX}: this check reads the live Dolt store and deliberately does NOT\n` +
      `${PREFIX}: fall back to .beads/issues.jsonl, which is a passive, scrubbed\n` +
      `${PREFIX}: export. No verdict about milestone labels is implied either way.\n`,
  );
  const text = detail?.trim();
  if (text) for (const line of text.split("\n")) process.stdout.write(`${PREFIX}:   ${line}\n`);
  return 0;
}

/** `value count` pairs, most frequent first. */
function tally(values: readonly unknown[]): string {
  const counts = new Map<string, number>();
  for (const value of values) counts.set(String(value), (counts.get(String(value)) ?? 0) + 1);
  return [...counts]
    .sort((left, right) => right[1] - left[1] || left[0].localeCompare(right[0]))
    .map(([value, count]) => `${value} ${count}`)
    .join(", ");
}

// Report the export's divergence from the store rather than trusting it. A
// reader who sees these two numbers differ cannot mistake the file for the
// tracker.
function exportNote(root: string, storeCount: string): string {
  try {
    const path = join(root, ".beads/issues.jsonl");
    const records = readFileSync(path, "utf8")
      .split("\n")
      .filter((line) => line.trim() !== "").length;
    return (
      `.beads/issues.jsonl holds ${records} records (store holds ${storeCount}); ` +
      `mtime ${statSync(path).mtime.toISOString()} is a checkout artifact, not an export time`
    );
  } catch {
    // An absent export is fine: this check never reads it for the verdict.
    return ".beads/issues.jsonl absent";
  }
}

function describe(issue: Table): string {
  return `${PREFIX}:   ${String(issue["id"])}  [${String(issue["status"])}/${String(issue["issue_type"])}]  ${str(issue["title"]) ?? ""}\n`;
}

export function milestoneLabels(root = ROOT): number {
  // `bd` is optional tooling; a contributor without it is not a failure.
  let version: string;
  try {
    version = bd(["version"], root).trim();
  } catch {
    return skip("no `bd` on PATH");
  }

  // `bd info` is the provenance source: it names the store this check read.
  // Resolution walks up from the worktree, so in a linked worktree this is the
  // MAIN repository's store, not a per-worktree copy. Printing it is the point
  // -- the reader must be able to see exactly what was queried.
  let info: string;
  try {
    info = bd(["info"], root);
  } catch (error) {
    return skip("no local beads store", error instanceof Error ? error.message : String(error));
  }
  const field = (name: string): string =>
    new RegExp(`^${name}:\\s*(.+)$`, "m").exec(info)?.[1]?.trim() ?? "(unreported)";
  const storeCount = field("Issue Count");

  // --limit 0 is unlimited; the default is 50 and would silently truncate.
  // The three --include-* flags defeat bd's default hiding of gate,
  // infrastructure, and template beads -- this gate must not inherit someone
  // else's idea of what is worth showing.
  let issues: unknown;
  try {
    issues = JSON.parse(
      bd(
        [
          "list",
          "--all",
          "--limit",
          "0",
          "--include-gates",
          "--include-infra",
          "--include-templates",
          "--json",
        ],
        root,
      ),
    );
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    process.stderr.write(`${PREFIX}: could not read the beads store\n${reason}\n`);
    return 1;
  }
  if (!Array.isArray(issues)) {
    process.stderr.write(`${PREFIX}: expected a JSON array from \`bd list --json\`\n`);
    return 1;
  }

  const live = issues.map(table).filter((issue) => issue["status"] !== "closed");
  process.stdout.write(
    `${PREFIX}: read the LIVE beads store; the tracked JSONL export was NOT consulted\n` +
      `${PREFIX}:   bd:            ${version}\n` +
      `${PREFIX}:   store:         ${field("Database")} (mode: ${field("Mode")})\n` +
      `${PREFIX}:   records:       ${storeCount} total, ${live.length} non-closed\n` +
      `${PREFIX}:   statuses:      ${tally(live.map((issue) => issue["status"]))}\n` +
      `${PREFIX}:   types:         ${tally(live.map((issue) => issue["issue_type"]))}\n` +
      `${PREFIX}:   export (skipped): ${exportNote(root, storeCount)}\n`,
  );

  const labelsOf = (issue: Table): string[] =>
    MILESTONES.filter((label) => strings(issue["labels"]).includes(label));
  const missing = live.filter((issue) => labelsOf(issue).length === 0);
  const doubled = live.filter((issue) => labelsOf(issue).length > 1);

  if (missing.length === 0 && doubled.length === 0) {
    const rc = live.filter((issue) => strings(issue["labels"]).includes("rc-1.0")).length;
    process.stdout.write(
      `${PREFIX}: OK -- all ${live.length} non-closed beads carry exactly one milestone ` +
        `label (${rc} rc-1.0, ${live.length - rc} post-1.0)\n`,
    );
    return 0;
  }
  if (missing.length > 0) {
    process.stderr.write(
      `${PREFIX}: ${missing.length} non-closed bead(s) carry neither rc-1.0 nor post-1.0, ` +
        `so the 1.0 cut line undercounts by that much:\n`,
    );
    for (const issue of missing) process.stderr.write(describe(issue));
  }
  if (doubled.length > 0) {
    process.stderr.write(
      `${PREFIX}: ${doubled.length} non-closed bead(s) carry BOTH milestone labels:\n`,
    );
    for (const issue of doubled) process.stderr.write(describe(issue));
  }
  process.stderr.write(
    `${PREFIX}: label each with \`bd update <id> --add-label rc-1.0\` (or post-1.0).\n` +
      `${PREFIX}: criterion (phux-i7vu, from ADR-0071 point 1): rc-1.0 when it changes a\n` +
      `${PREFIX}: frozen surface -- CLI grammar, exit codes, --json documents, config\n` +
      `${PREFIX}: schema, action/hook/widget vocabulary, MCP tool names and arguments,\n` +
      `${PREFIX}: documented file and socket locations -- or when leaving it undone makes\n` +
      `${PREFIX}: one of those surfaces wrong at the freeze. post-1.0 otherwise.\n`,
  );
  return 1;
}
