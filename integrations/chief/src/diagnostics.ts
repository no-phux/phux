import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { closeSync, existsSync, openSync, readdirSync, readFileSync, readSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

/**
 * The chief's error intake: the places a fault in this system shows up,
 * read on a timer and reduced to fingerprinted findings. New findings go to
 * the chief as a turn (self-heal `auto`) or a note (`notify`). The model
 * itself only ever has phux and web tools; this reading happens host-side.
 */
export interface Finding {
  readonly fingerprint: string;
  readonly source: "phux-server" | "crash-report" | "phux-report" | "ci" | "chief" | "mobile";
  readonly title: string;
  readonly detail: string;
}

export interface IntakeState {
  serverLogOffset?: number;
  serverLogInode?: number;
  seen: string[];
  lastScan?: number;
  lastInvestigation?: number;
}

export interface IntakeOptions {
  readonly statePath: string;
  readonly ownedRepos: readonly string[];
  readonly serverLog?: string;
  readonly crashDir?: string;
  readonly reportsDir?: string;
  readonly run?: (file: string, args: readonly string[]) => Promise<string>;
  readonly now?: () => number;
}

const MAX_SEEN = 2_000;
const MAX_LOG_READ = 4 * 1024 * 1024;
const MAX_FINDINGS_PER_SCAN = 8;

export class ErrorIntake {
  private state: IntakeState;
  private readonly run: (file: string, args: readonly string[]) => Promise<string>;
  private readonly now: () => number;
  private readonly startedAt: number;

  constructor(private readonly options: IntakeOptions) {
    this.state = loadState(options.statePath);
    this.run = options.run ?? execText;
    this.now = options.now ?? Date.now;
    // A first run starts quiet: only what happens from now on is news.
    this.startedAt = this.state.lastScan ?? this.now();
  }

  /** Every source once; returns only findings not seen before, at most a handful. */
  async scan(): Promise<Finding[]> {
    const since = this.state.lastScan ?? this.startedAt;
    const found: Finding[] = [];
    const sources: (() => Promise<Finding[]> | Finding[])[] = [
      () => this.serverLog(),
      () => this.newFiles(this.options.crashDir ?? join(homedir(), "Library", "Logs", "DiagnosticReports"), /^phux.*\.ips$/, "crash-report", since),
      () => this.newFiles(this.options.reportsDir ?? join(homedir(), ".local", "state", "phux", "reports"), /./, "phux-report", since),
      () => this.ci(since),
    ];
    for (const source of sources) {
      try {
        found.push(...await source());
      } catch (error) {
        found.push(chiefFinding(`intake source failed: ${error instanceof Error ? error.message : String(error)}`));
      }
    }
    this.state.lastScan = this.now();
    const fresh = this.admit(found);
    this.save();
    return fresh;
  }

  /** Remember `findings` and return the ones not seen before. */
  admit(findings: readonly Finding[]): Finding[] {
    const seen = new Set(this.state.seen);
    const fresh: Finding[] = [];
    for (const finding of findings) {
      if (seen.has(finding.fingerprint)) continue;
      seen.add(finding.fingerprint);
      this.state.seen.push(finding.fingerprint);
      if (fresh.length < MAX_FINDINGS_PER_SCAN) fresh.push(finding);
    }
    if (this.state.seen.length > MAX_SEEN) this.state.seen = this.state.seen.slice(-MAX_SEEN);
    this.save();
    return fresh;
  }

  /** Whether an automatic investigation may start now; records it when so. */
  claimInvestigation(cooldownMinutes: number): boolean {
    const last = this.state.lastInvestigation;
    if (last !== undefined && this.now() - last < cooldownMinutes * 60_000) return false;
    this.state.lastInvestigation = this.now();
    this.save();
    return true;
  }

  private serverLog(): Finding[] {
    const path = this.options.serverLog ?? join(homedir(), ".local", "state", "phux", "server.log");
    if (!existsSync(path)) return [];
    const stat = statSync(path);
    let offset = this.state.serverLogOffset ?? stat.size;
    // Rotated or truncated: start over on the new file.
    if (this.state.serverLogInode !== undefined && (this.state.serverLogInode !== stat.ino || stat.size < offset)) offset = 0;
    const length = Math.min(stat.size - offset, MAX_LOG_READ);
    const text = length > 0 ? readRange(path, stat.size - length, length) : "";
    this.state.serverLogOffset = stat.size;
    this.state.serverLogInode = stat.ino;
    return serverLogFindings(text);
  }

  private newFiles(dir: string, pattern: RegExp, source: Finding["source"], since: number): Finding[] {
    if (!existsSync(dir)) return [];
    return readdirSync(dir)
      .filter((name) => pattern.test(name))
      .map((name) => ({ name, mtime: statSync(join(dir, name)).mtimeMs }))
      .filter((file) => file.mtime > since)
      .map((file) => ({
        fingerprint: `${source}:${file.name}`,
        source,
        title: `${source === "crash-report" ? "macOS crash report" : "phux bug report"} ${file.name}`,
        detail: `${join(dir, file.name)} (written ${new Date(file.mtime).toISOString()})`,
      }));
  }

  private async ci(since: number): Promise<Finding[]> {
    const findings: Finding[] = [];
    for (const repo of this.options.ownedRepos) {
      const json = await this.run("gh", ["run", "list", "--repo", repo, "--branch", "main", "--status", "failure", "--limit", "5",
        "--json", "databaseId,workflowName,displayTitle,url,createdAt,headSha"]);
      const runs = JSON.parse(json) as { databaseId: number; workflowName: string; displayTitle: string; url: string; createdAt: string; headSha: string }[];
      for (const run of runs) {
        if (Date.parse(run.createdAt) < since - 3600_000) continue;
        findings.push({
          fingerprint: `ci:${repo}:${run.databaseId}`,
          source: "ci",
          title: `${repo} main: ${run.workflowName} failed`,
          detail: `${run.displayTitle} @ ${run.headSha.slice(0, 10)} — ${run.url}`,
        });
      }
    }
    return findings;
  }

  private save(): void {
    try {
      writeFileSync(this.options.statePath, JSON.stringify(this.state));
    } catch {
      // A read-only home must not stop the chief; findings just repeat.
    }
  }
}

/** ERROR lines and panics, one finding per distinct message template. */
export function serverLogFindings(text: string): Finding[] {
  const out = new Map<string, Finding>();
  const lines = text.split("\n");
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i]!;
    const error = /^\S+\s+ERROR\s+(.*)$/.exec(line);
    const panic = /panicked at (.*)/.exec(line);
    if (error === null && panic === null) continue;
    const message = (error?.[1] ?? `panic ${panic![1]}`).trim();
    const fingerprint = `phux-server:${hash(template(message))}`;
    if (out.has(fingerprint)) continue;
    out.set(fingerprint, {
      fingerprint,
      source: "phux-server",
      title: `phux server: ${message.slice(0, 160)}`,
      detail: lines.slice(Math.max(0, i - 2), i + 6).join("\n").slice(0, 2_000),
    });
  }
  return [...out.values()];
}

/** A message with its variable parts (numbers, ids, paths, quoted values) blanked. */
export function template(message: string): string {
  return message
    .replace(/time\.(busy|idle)=\S+/g, "")
    .replace(/"[^"]*"/g, "\"_\"")
    .replace(/\/[\w./-]+/g, "/_")
    .replace(/\b0x[0-9a-f]+\b/gi, "N")
    .replace(/\b[0-9a-f]{8,}\b/gi, "N")
    .replace(/\d+/g, "N")
    .replace(/\s+/g, " ")
    .trim();
}

export function chiefFinding(message: string): Finding {
  return { fingerprint: `chief:${hash(template(message))}`, source: "chief", title: `chief: ${message.slice(0, 160)}`, detail: message.slice(0, 2_000) };
}

/** The prompt an automatic investigation starts with. */
export function investigationPrompt(findings: readonly Finding[], ownedRepos: readonly string[]): string {
  return [
    "[error intake] New errors were observed in the system. Investigate them with your phux tools (open a shell pane; read logs, reproduce, inspect code).",
    "If the root cause is in a repository you own, fix it on a branch in a dedicated git worktree, run the repository's checks, push, and open a PR. Never push to main, never restart or kill the phux server, never kill processes you did not start.",
    `Owned repositories: ${ownedRepos.length > 0 ? ownedRepos.join(", ") : "see your persona"}.`,
    "If it is noise or not actionable, say so in one line. Finish with a short report: what failed, the cause, what you did, PR links.",
    "",
    ...findings.map((finding, index) => `${index + 1}. [${finding.source}] ${finding.title}\n${finding.detail}`),
  ].join("\n");
}

function hash(text: string): string {
  return createHash("sha256").update(text).digest("hex").slice(0, 16);
}

function readRange(path: string, start: number, length: number): string {
  const fd = openSync(path, "r");
  try {
    const buffer = Buffer.alloc(length);
    readSync(fd, buffer, 0, length, start);
    return buffer.toString("utf8");
  } finally {
    closeSync(fd);
  }
}

function loadState(path: string): IntakeState {
  try {
    const state = JSON.parse(readFileSync(path, "utf8")) as IntakeState;
    return Array.isArray(state.seen) ? state : { seen: [] };
  } catch {
    return { seen: [] };
  }
}

function execText(file: string, args: readonly string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(file, [...args], { timeout: 30_000, maxBuffer: 4 * 1024 * 1024 }, (error, stdout) => {
      if (error) reject(error);
      else resolve(stdout);
    });
  });
}
