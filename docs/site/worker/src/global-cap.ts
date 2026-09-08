import { DurableObject } from "cloudflare:workers";
import {
  accountLimitReason,
  accountNativeDeadline,
  decideCircuit,
  EMPTY_CIRCUIT,
  nativeCapacityReason,
  recordCircuitFailure,
  recordCircuitSuccess,
  splitUtcUsage,
  utcDay,
  type AccountQuotaConfig,
  type CircuitConfig,
  type CircuitState,
} from "./native-admission";

// GlobalCapDO — a single well-known DO instance (name "global") that owns:
//   • global and native concurrency reservations,
//   • provider-principal native launch/runtime budgets.
//
// Routing every reserve/release through one object makes the cap strongly
// consistent: there is exactly one authority, so simultaneous launches cannot
// race past an account or capacity limit.
//
// Robustness: every reservation carries an expiry. If a SessionDO never
// releases (crash, lost socket, eviction between reserve and connect), an alarm
// reaps the stale slot. The counter is therefore self-healing — a leaked slot
// frees itself within one expiry window instead of permanently shrinking the
// demo's capacity.

interface Env {
  GLOBAL_CONCURRENCY_CAP: string;
}

// How often the reaper runs to sweep expired reservations / rate buckets.
const REAP_INTERVAL_MS = 15_000;
const CIRCUIT_KEY = "native-circuit";

export type NativeAdmission =
  | { backend: "native"; nativeUntil: number }
  | {
      backend: "edge";
      reason:
        | "auth-required"
        | "account-active"
        | "account-hourly-limit"
        | "account-daily-limit"
        | "native-capacity"
        | "per-ip-capacity"
        | "circuit-open"
        | "circuit-half-open";
    }
  | { backend: "reject"; reason: "global-capacity" };

export interface CapStatus {
  live: number;
  nativeLive: number;
  circuit: "closed" | "open" | "half-open";
  recentFailures: number;
  openUntil: string | null;
}

export class GlobalCapDO extends DurableObject<Env> {
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    ctx.blockConcurrencyWhile(async () => {
      this.ctx.storage.sql.exec(`
        CREATE TABLE IF NOT EXISTS slots (
          sid       TEXT PRIMARY KEY,
          expires   INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS native_slots (
          sid       TEXT PRIMARY KEY,
          ip        TEXT NOT NULL,
          expires   INTEGER NOT NULL,
          principal TEXT,
          admitted  INTEGER,
          native_until INTEGER
        );
        CREATE TABLE IF NOT EXISTS native_launches (
          sid       TEXT PRIMARY KEY,
          principal TEXT NOT NULL,
          ts        INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS native_usage (
          principal TEXT NOT NULL,
          day       TEXT NOT NULL,
          used_ms   INTEGER NOT NULL,
          PRIMARY KEY (principal, day)
        );
        CREATE INDEX IF NOT EXISTS native_slots_ip ON native_slots (ip);
        CREATE INDEX IF NOT EXISTS native_launches_principal_ts
          ON native_launches (principal, ts);
      `);
      // Rate windows moved to secret-keyed per-IP objects in migration v3.
      // Remove the legacy raw-IP table instead of retaining stale addresses.
      this.ctx.storage.sql.exec("DROP INDEX IF EXISTS hits_ip_ts");
      this.ctx.storage.sql.exec("DROP TABLE IF EXISTS hits");
      const nativeColumns = new Set(
        this.ctx.storage.sql
          .exec<{ name: string }>("PRAGMA table_info(native_slots)")
          .toArray()
          .map((column) => column.name),
      );
      if (!nativeColumns.has("principal"))
        this.ctx.storage.sql.exec("ALTER TABLE native_slots ADD COLUMN principal TEXT");
      if (!nativeColumns.has("admitted"))
        this.ctx.storage.sql.exec("ALTER TABLE native_slots ADD COLUMN admitted INTEGER");
      if (!nativeColumns.has("native_until"))
        this.ctx.storage.sql.exec("ALTER TABLE native_slots ADD COLUMN native_until INTEGER");
      this.ctx.storage.sql.exec(
        "CREATE INDEX IF NOT EXISTS native_slots_principal ON native_slots (principal)",
      );
    });
  }

  // Reserve a concurrency slot for `sid`. Returns false when the cap is hit.
  // Idempotent on `sid`: re-reserving an existing slot just refreshes its TTL.
  async reserve(sid: string, cap: number, ttlMs: number): Promise<boolean> {
    const now = Date.now();
    this.sweepExpired(now);

    const existing = this.ctx.storage.sql
      .exec<{ n: number }>("SELECT COUNT(*) AS n FROM slots WHERE sid = ?", sid)
      .one().n;

    if (existing === 0) {
      const live = this.ctx.storage.sql
        .exec<{ n: number }>("SELECT COUNT(*) AS n FROM slots").one().n;
      if (live >= cap) return false;
    }

    this.ctx.storage.sql.exec(
      "INSERT INTO slots (sid, expires) VALUES (?, ?) " +
        "ON CONFLICT(sid) DO UPDATE SET expires = excluded.expires",
      sid,
      now + ttlMs,
    );
    await this.ensureAlarm();
    return true;
  }

  // Native shells are materially more expensive than edge sessions. Reserve
  // their global slot, native capacity, IP cap, and identity budget atomically.
  async admitNative(
    sid: string,
    ip: string,
    principal: string,
    globalCap: number,
    nativeCap: number,
    perIpCap: number,
    accountConfig: AccountQuotaConfig,
    hardMaxMs: number,
    ttlMs: number,
    edgeTtlMs: number,
    circuitConfig: CircuitConfig,
  ): Promise<NativeAdmission> {
    return this.ctx.blockConcurrencyWhile(async () => {
      const now = Date.now();
      this.sweepExpired(now);

      const existingNative = this.ctx.storage.sql
        .exec<{ native_until: number | null }>(
          "SELECT native_until FROM native_slots WHERE sid = ? AND principal = ?",
          sid,
          principal,
        )
        .toArray()[0];
      if (existingNative) {
        return {
          backend: "native",
          nativeUntil: existingNative.native_until ?? now + hardMaxMs,
        };
      }

      const existing = this.ctx.storage.sql
        .exec<{ n: number }>("SELECT COUNT(*) AS n FROM slots WHERE sid = ?", sid)
        .one().n;
      if (existing === 0) {
        const live = this.ctx.storage.sql
          .exec<{ n: number }>("SELECT COUNT(*) AS n FROM slots")
          .one().n;
        const nativeLive = this.ctx.storage.sql
          .exec<{ n: number }>("SELECT COUNT(*) AS n FROM native_slots")
          .one().n;
        const ipLive = this.ctx.storage.sql
          .exec<{ n: number }>(
            "SELECT COUNT(*) AS n FROM native_slots WHERE ip = ?",
            ip,
          )
          .one().n;
        if (live >= globalCap)
          return { backend: "reject", reason: "global-capacity" };

        const fallbackReason = nativeCapacityReason(
          nativeLive,
          nativeCap,
          ipLive,
          perIpCap,
        );
        if (fallbackReason) {
          this.putSlot(sid, now + edgeTtlMs);
          return { backend: "edge", reason: fallbackReason };
        }
      }

      this.ctx.storage.sql.exec(
        "DELETE FROM native_launches WHERE ts <= ?",
        now - 60 * 60_000,
      );
      const accountActive = this.ctx.storage.sql
        .exec<{ n: number }>(
          "SELECT COUNT(*) AS n FROM native_slots WHERE principal = ?",
          principal,
        )
        .one().n;
      const launches = this.ctx.storage.sql
        .exec<{ n: number }>(
          "SELECT COUNT(*) AS n FROM native_launches WHERE principal = ?",
          principal,
        )
        .one().n;
      const usageByDay = this.accountUsage(principal, now);
      const accountReason = accountLimitReason(
        accountActive,
        launches,
        usageByDay.get(utcDay(now)) ?? 0,
        accountConfig,
      );
      if (accountReason) {
        this.putSlot(sid, now + edgeTtlMs);
        return { backend: "edge", reason: accountReason };
      }

      const current =
        (await this.ctx.storage.get<CircuitState>(CIRCUIT_KEY)) ?? EMPTY_CIRCUIT;
      const decision = decideCircuit(current, sid, now, circuitConfig);
      await this.ctx.storage.put(CIRCUIT_KEY, decision.state);
      if (!decision.allow) {
        this.putSlot(sid, now + edgeTtlMs);
        return { backend: "edge", reason: decision.reason };
      }

      const nativeUntil = accountNativeDeadline(
        now,
        hardMaxMs,
        accountConfig.dailyMs,
        usageByDay,
      );
      const expires = now + ttlMs;
      this.ctx.storage.transactionSync(() => {
        this.ctx.storage.sql.exec(
          "INSERT INTO slots (sid, expires) VALUES (?, ?) " +
            "ON CONFLICT(sid) DO UPDATE SET expires = excluded.expires",
          sid,
          expires,
        );
        this.ctx.storage.sql.exec(
          "INSERT INTO native_slots (sid, ip, expires, principal, admitted, native_until) " +
            "VALUES (?, ?, ?, ?, ?, ?)",
          sid,
          ip,
          expires,
          principal,
          now,
          nativeUntil,
        );
        this.ctx.storage.sql.exec(
          "INSERT INTO native_launches (sid, principal, ts) VALUES (?, ?, ?)",
          sid,
          principal,
          now,
        );
      });
      this.ctx.waitUntil(this.ensureAlarm());
      return { backend: "native", nativeUntil };
    });
  }

  async nativeFailed(
    sid: string,
    edgeTtlMs: number,
    circuitConfig: CircuitConfig,
  ): Promise<void> {
    const now = Date.now();
    this.finalizeNative(sid, now);
    this.ctx.storage.sql.exec(
      "UPDATE slots SET expires = ? WHERE sid = ?",
      now + edgeTtlMs,
      sid,
    );
    const current =
      (await this.ctx.storage.get<CircuitState>(CIRCUIT_KEY)) ?? EMPTY_CIRCUIT;
    await this.ctx.storage.put(
      CIRCUIT_KEY,
      recordCircuitFailure(current, sid, now, circuitConfig),
    );
  }

  async nativeSucceeded(sid: string): Promise<void> {
    const native = this.ctx.storage.sql
      .exec<{ n: number }>(
        "SELECT COUNT(*) AS n FROM native_slots WHERE sid = ?",
        sid,
      )
      .one().n;
    if (native > 0)
      await this.ctx.storage.put(CIRCUIT_KEY, recordCircuitSuccess());
  }

  // Container stop callbacks can arrive after a failed session was downgraded.
  // Delete the global slot only while it is still marked native.
  async releaseNative(sid: string): Promise<void> {
    this.sweepExpired(Date.now());
    if (!this.finalizeNative(sid, Date.now())) return;
    this.ctx.storage.sql.exec("DELETE FROM slots WHERE sid = ?", sid);
  }

  // Extend an active reservation's TTL — called by the SessionDO heartbeat so a
  // long-but-live session is never reaped out from under itself.
  async heartbeat(sid: string, ttlMs: number): Promise<void> {
    this.ctx.storage.sql.exec(
      "UPDATE slots SET expires = ? WHERE sid = ?",
      Date.now() + ttlMs,
      sid,
    );
  }

  // Release a slot on session teardown. Idempotent.
  async release(sid: string): Promise<void> {
    this.finalizeNative(sid, Date.now());
    this.ctx.storage.sql.exec("DELETE FROM slots WHERE sid = ?", sid);
  }

  async status(config: CircuitConfig): Promise<CapStatus> {
    const now = Date.now();
    this.sweepExpired(now);
    const state =
      (await this.ctx.storage.get<CircuitState>(CIRCUIT_KEY)) ?? EMPTY_CIRCUIT;
    const decision = decideCircuit(state, "diagnostic-only", now, config);
    const circuit =
      state.openUntil > now
        ? "open"
        : state.openUntil > 0
          ? "half-open"
          : "closed";
    return {
      live: this.ctx.storage.sql
        .exec<{ n: number }>("SELECT COUNT(*) AS n FROM slots")
        .one().n,
      nativeLive: this.ctx.storage.sql
        .exec<{ n: number }>("SELECT COUNT(*) AS n FROM native_slots")
        .one().n,
      circuit,
      recentFailures: decision.state.failures.length,
      openUntil:
        state.openUntil > now ? new Date(state.openUntil).toISOString() : null,
    };
  }

  // Diagnostics: current live slot count (used by /healthz-style probes if wired).
  async liveCount(): Promise<number> {
    this.sweepExpired(Date.now());
    return this.ctx.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM slots").one().n;
  }

  private sweepExpired(now: number): void {
    for (const row of this.ctx.storage.sql
      .exec<{ sid: string; expires: number }>(
        "SELECT sid, expires FROM native_slots WHERE expires <= ?",
        now,
      )
      .toArray()) {
      this.finalizeNative(row.sid, row.expires);
    }
    this.ctx.storage.sql.exec("DELETE FROM slots WHERE expires <= ?", now);
    this.ctx.storage.sql.exec(
      "DELETE FROM native_launches WHERE ts <= ?",
      now - 60 * 60_000,
    );
    // Daily quotas need only today's aggregate; older identity accounting is
    // deleted rather than turning this admission object into a user-history DB.
    this.ctx.storage.sql.exec(
      "DELETE FROM native_usage WHERE day < ?",
      utcDay(now),
    );
  }

  private accountUsage(principal: string, now: number): Map<string, number> {
    const usage = new Map(
      this.ctx.storage.sql
        .exec<{ day: string; used_ms: number }>(
          "SELECT day, used_ms FROM native_usage WHERE principal = ?",
          principal,
        )
        .toArray()
        .map((row) => [row.day, row.used_ms] as const),
    );
    for (const row of this.ctx.storage.sql
      .exec<{ admitted: number | null }>(
        "SELECT admitted FROM native_slots WHERE principal = ?",
        principal,
      )
      .toArray()) {
      if (row.admitted === null) continue;
      for (const slice of splitUtcUsage(row.admitted, now)) {
        usage.set(slice.day, (usage.get(slice.day) ?? 0) + slice.durationMs);
      }
    }
    return usage;
  }

  private finalizeNative(sid: string, ended: number): boolean {
    return this.ctx.storage.transactionSync(() => {
      const row = this.ctx.storage.sql
        .exec<{ principal: string | null; admitted: number | null }>(
          "SELECT principal, admitted FROM native_slots WHERE sid = ?",
          sid,
        )
        .toArray()[0];
      if (!row) return false;
      if (row.principal !== null && row.admitted !== null) {
        for (const slice of splitUtcUsage(row.admitted, ended)) {
          this.ctx.storage.sql.exec(
            "INSERT INTO native_usage (principal, day, used_ms) VALUES (?, ?, ?) " +
              "ON CONFLICT(principal, day) DO UPDATE SET used_ms = used_ms + excluded.used_ms",
            row.principal,
            slice.day,
            slice.durationMs,
          );
        }
      }
      this.ctx.storage.sql.exec("DELETE FROM native_slots WHERE sid = ?", sid);
      return true;
    });
  }

  private putSlot(sid: string, expires: number): void {
    this.ctx.storage.sql.exec(
      "INSERT INTO slots (sid, expires) VALUES (?, ?) " +
        "ON CONFLICT(sid) DO UPDATE SET expires = excluded.expires",
      sid,
      expires,
    );
    this.ctx.waitUntil(this.ensureAlarm());
  }

  private async ensureAlarm(): Promise<void> {
    const current = await this.ctx.storage.getAlarm();
    if (current === null) {
      await this.ctx.storage.setAlarm(Date.now() + REAP_INTERVAL_MS);
    }
  }

  // Reaper: sweep expired slots. Reschedules itself only
  // while there is still bookkeeping to maintain, so the DO can go idle/billing-
  // pause once everything is clean.
  async alarm(): Promise<void> {
    const now = Date.now();
    this.sweepExpired(now);
    const remaining = this.ctx.storage.sql
      .exec<{ n: number }>("SELECT COUNT(*) AS n FROM slots")
      .one().n;
    if (remaining > 0) {
      await this.ctx.storage.setAlarm(now + REAP_INTERVAL_MS);
    }
  }
}
