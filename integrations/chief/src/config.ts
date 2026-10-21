import { existsSync, readFileSync } from "node:fs";
import { homedir, hostname } from "node:os";
import { join } from "node:path";

/** What `chief.json` may set. Everything is optional; absent means the default. */
export interface ChiefConfig {
  /** The one host in the mesh that runs the chief; `ensure` is a no-op elsewhere. Default: this host. */
  readonly host: string;
  /** `provider/modelId` a new conversation starts with; `/model` changes it later. */
  readonly model: string;
  readonly thinking: "off" | "minimal" | "low" | "medium" | "high" | "xhigh";
  /**
   * Run messages the phone sent before Face ID unlock right away, tools included.
   * Off: they are held, unseen by the model, until the phone sends `unlock`.
   */
  readonly yolo: boolean;
  /** Put the reply text in the turn-complete push (`stop.data.reply`). */
  readonly pushReplyText: boolean;
  /** `auto` investigates new errors on its own; `notify` only reports them; `off` ignores them. */
  readonly selfHeal: "auto" | "notify" | "off";
  /** Minimum minutes between two automatic investigations. */
  readonly selfHealCooldownMinutes: number;
  /** Repositories the chief may branch, push, and open PRs in (`owner/name`). */
  readonly ownedRepos: readonly string[];
  /** pi's `auth.json`. Shared with pi by default so OAuth refresh stays serialized by its file lock. */
  readonly authPath: string;
  /** phux session the chief pane lives in. */
  readonly session: string;
  /** Working directory for the chief process and its tools. */
  readonly cwd: string;
}

export interface ChiefPaths {
  /** State: the durable database, intake memory, log, and the `disabled` switch. */
  readonly home: string;
  readonly config: string;
  /** Optional persona addendum, appended to the shipped persona. */
  readonly persona: string;
  /** Optional extra skills (`*.md`), beside the shipped ones. */
  readonly skills: string;
  readonly database: string;
  readonly diagnostics: string;
  readonly log: string;
  /** While this file exists, `ensure` starts nothing. */
  readonly disabled: string;
}

export function chiefPaths(env: NodeJS.ProcessEnv = process.env): ChiefPaths {
  const configHome = env.XDG_CONFIG_HOME?.trim() || join(homedir(), ".config");
  const stateHome = env.XDG_STATE_HOME?.trim() || join(homedir(), ".local", "state");
  const home = env.PHUX_CHIEF_HOME?.trim() || join(stateHome, "phux", "chief");
  return {
    home,
    config: env.PHUX_CHIEF_CONFIG?.trim() || join(configHome, "phux", "chief.json"),
    persona: join(configHome, "phux", "chief.md"),
    skills: join(configHome, "phux", "chief-skills"),
    database: join(home, "chief.sqlite"),
    diagnostics: join(home, "intake.json"),
    log: join(home, "chief.log"),
    disabled: join(home, "disabled"),
  };
}

export function defaultConfig(env: NodeJS.ProcessEnv = process.env): ChiefConfig {
  return {
    host: shortHost(hostname()),
    model: "anthropic/claude-sonnet-5",
    thinking: "medium",
    yolo: false,
    pushReplyText: true,
    selfHeal: "auto",
    selfHealCooldownMinutes: 60,
    ownedRepos: [],
    authPath: join(env.PI_CODING_AGENT_DIR?.trim() || join(homedir(), ".pi", "agent"), "auth.json"),
    session: "chief",
    cwd: homedir(),
  };
}

const THINKING = new Set(["off", "minimal", "low", "medium", "high", "xhigh"]);
const SELF_HEAL = new Set(["auto", "notify", "off"]);

/** Merge a parsed `chief.json` over the defaults, refusing ill-typed fields by name. */
export function parseConfig(raw: unknown, base: ChiefConfig): ChiefConfig {
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) throw new Error("chief.json must be a JSON object");
  const value = raw as Record<string, unknown>;
  const out: Record<string, unknown> = { ...base };
  const string = (key: keyof ChiefConfig) => {
    if (value[key] === undefined) return;
    if (typeof value[key] !== "string" || (value[key] as string).trim() === "") throw new Error(`chief.json: ${key} must be a non-empty string`);
    out[key] = (value[key] as string).trim();
  };
  const boolean = (key: keyof ChiefConfig) => {
    if (value[key] === undefined) return;
    if (typeof value[key] !== "boolean") throw new Error(`chief.json: ${key} must be true or false`);
    out[key] = value[key];
  };
  for (const key of ["host", "model", "authPath", "session", "cwd"] as const) string(key);
  for (const key of ["yolo", "pushReplyText"] as const) boolean(key);
  if (value.thinking !== undefined) {
    if (!THINKING.has(value.thinking as string)) throw new Error(`chief.json: thinking must be one of ${[...THINKING].join(", ")}`);
    out.thinking = value.thinking;
  }
  if (value.selfHeal !== undefined) {
    if (!SELF_HEAL.has(value.selfHeal as string)) throw new Error("chief.json: selfHeal must be auto, notify, or off");
    out.selfHeal = value.selfHeal;
  }
  if (value.selfHealCooldownMinutes !== undefined) {
    const minutes = value.selfHealCooldownMinutes;
    if (typeof minutes !== "number" || !Number.isFinite(minutes) || minutes < 0) throw new Error("chief.json: selfHealCooldownMinutes must be a non-negative number");
    out.selfHealCooldownMinutes = minutes;
  }
  if (value.ownedRepos !== undefined) {
    const repos = value.ownedRepos;
    if (!Array.isArray(repos) || !repos.every((repo) => typeof repo === "string" && /^[A-Za-z0-9][A-Za-z0-9-]*\/(?!\.\.?$)[\w.-]+$/.test(repo))) {
      throw new Error("chief.json: ownedRepos must be an array of owner/name strings");
    }
    out.ownedRepos = repos;
  }
  if (!/^[^/\s]+\/\S+$/.test(out.model as string)) throw new Error("chief.json: model must be provider/modelId");
  return out as unknown as ChiefConfig;
}

export function loadConfig(paths: ChiefPaths, env: NodeJS.ProcessEnv = process.env): ChiefConfig {
  const base = defaultConfig(env);
  const config = existsSync(paths.config) ? parseConfig(JSON.parse(readFileSync(paths.config, "utf8")), base) : base;
  // The process flag wins over the file so a supervisor can force either.
  if (env.PHUX_CHIEF_YOLO === "1") return { ...config, yolo: true };
  if (env.PHUX_CHIEF_YOLO === "0") return { ...config, yolo: false };
  return config;
}

/** `mac.tail1354da.ts.net` and `mac.local` both name host `mac`. */
export function shortHost(name: string): string {
  return name.split(".")[0]!.toLowerCase();
}

/** Split `provider/modelId`; model ids may themselves contain slashes. */
export function splitModel(ref: string): { provider: string; modelId: string } {
  const at = ref.indexOf("/");
  if (at <= 0 || at === ref.length - 1) throw new Error(`expected provider/modelId, got ${JSON.stringify(ref)}`);
  return { provider: ref.slice(0, at), modelId: ref.slice(at + 1) };
}

/** While `paths.disabled` exists, neither `ensure` nor the supervisor starts a chief. */
export function chiefDisabled(paths: ChiefPaths): boolean {
  return existsSync(paths.disabled);
}
