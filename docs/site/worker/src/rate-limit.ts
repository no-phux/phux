import { DurableObject } from "cloudflare:workers";
export { rateLimitName } from "./rate-limit-key";

const WINDOW_MS = 60_000;
interface Env {}

export class RateLimitDO extends DurableObject<Env> {
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.ctx.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS hits (ts INTEGER NOT NULL)",
    );
  }

  async check(limit: number): Promise<boolean> {
    const now = Date.now();
    const safeLimit = Number.isFinite(limit) ? Math.max(1, Math.floor(limit)) : 1;
    this.ctx.storage.sql.exec("DELETE FROM hits WHERE ts <= ?", now - WINDOW_MS);
    const count = this.ctx.storage.sql
      .exec<{ n: number }>("SELECT COUNT(*) AS n FROM hits")
      .one().n;
    if (count >= safeLimit) return false;
    this.ctx.storage.sql.exec("INSERT INTO hits (ts) VALUES (?)", now);
    await this.ctx.storage.setAlarm(now + WINDOW_MS);
    return true;
  }

  async alarm(): Promise<void> {
    this.ctx.storage.sql.exec("DELETE FROM hits");
  }
}
