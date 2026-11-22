import type { PhuxCli } from "../../runtime/src/adapter.js";

/** Tag on every pane this plugin spawns; `phux kill '#opencode-owned'` clears strays by hand. */
export const OWNED_TAG = "opencode-owned";
export const DEFAULT_MAX_TERMINALS = 4;

type TerminalCli = Pick<PhuxCli, "ls" | "kill" | "tag">;

/**
 * Panes spawned on behalf of one OpenCode session. The session owns them: they
 * count against a per-session cap, and they are killed when the session is
 * deleted or the plugin is disposed. Panes the agent did not spawn are never
 * killed here.
 */
export class OwnedTerminals {
  private readonly bySession = new Map<string, string[]>();

  constructor(
    private readonly cli: TerminalCli,
    private readonly limit = DEFAULT_MAX_TERMINALS,
  ) {
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 64) {
      throw new RangeError("maxTerminals must be an integer from 1 through 64");
    }
  }

  owned(sessionID: string): readonly string[] {
    return this.bySession.get(sessionID) ?? [];
  }

  /** Refuse a spawn over the cap, after forgetting local panes that already exited. */
  async admit(sessionID: string, signal?: AbortSignal): Promise<void> {
    const owned = this.owned(sessionID);
    if (owned.length < this.limit) return;
    const listed = (await this.cli.ls({ timeoutMs: 10_000, ...(signal === undefined ? {} : { signal }) })).terminals;
    const live = new Set(listed);
    // Satellite panes are absent from the local listing, and a v1 server lists
    // no terminals at all; keep anything the listing cannot vouch for.
    const remaining = listed.length === 0 ? [...owned] : owned.filter((target) => target.includes("/") || live.has(target));
    this.set(sessionID, remaining);
    if (remaining.length < this.limit) return;
    throw new Error(
      `This session already owns ${String(remaining.length)} phux terminals (${remaining.join(", ")}). ` +
        "Reuse one, or close one with phux_kill before spawning another.",
    );
  }

  async add(sessionID: string, target: string): Promise<void> {
    this.set(sessionID, [...this.owned(sessionID).filter((owned) => owned !== target), target]);
    // Tagging is a recovery aid for crashes, not part of ownership.
    await this.cli.tag("add", target, [OWNED_TAG], { timeoutMs: 5_000 }).catch(() => undefined);
  }

  async kill(sessionID: string, target: string, signal?: AbortSignal): Promise<void> {
    const owned = this.owned(sessionID);
    if (!owned.includes(target)) {
      throw new Error(`${target} was not spawned by this session; phux_kill only closes terminals this session owns (${owned.join(", ") || "none"}).`);
    }
    await this.cli.kill(target, { timeoutMs: 10_000, ...(signal === undefined ? {} : { signal }) }).catch((error: unknown) => {
      if (!isGone(error)) throw error;
    });
    this.set(sessionID, owned.filter((entry) => entry !== target));
  }

  /** Kill every terminal the session owns. Failures are best-effort: the pane may already be gone. */
  async release(sessionID: string): Promise<void> {
    const owned = this.owned(sessionID);
    this.bySession.delete(sessionID);
    await Promise.all(owned.map((target) => this.cli.kill(target, { timeoutMs: 5_000 }).catch(() => undefined)));
  }

  async releaseAll(): Promise<void> {
    await Promise.all([...this.bySession.keys()].map((sessionID) => this.release(sessionID)));
  }

  private set(sessionID: string, targets: string[]): void {
    if (targets.length === 0) this.bySession.delete(sessionID);
    else this.bySession.set(sessionID, targets);
  }
}

function isGone(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return /not found|no such|does not exist|unknown (pane|terminal)/i.test(message);
}
