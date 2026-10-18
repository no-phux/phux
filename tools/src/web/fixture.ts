// Test-only local owner for the browser's AgentSession lifecycle test. The
// browser cannot produce agent records; only the disposable server's Unix
// owner can. This loopback fixture offers canned actions, never arbitrary
// commands, behind an unguessable per-run path.
import { randomBytes } from "node:crypto";
import { describe, Failure } from "../lib/command.ts";
import { capture } from "../lib/process.ts";

export class AgentFixture {
  readonly token = randomBytes(24).toString("base64url");
  opened = false;
  // Actions run synchronously, so requests are served one at a time like the
  // single-threaded owner this stands in for.
  private readonly server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch: (request) => this.handle(request),
  });

  constructor(
    readonly binary: string,
    readonly socketPath: string,
    readonly env: Record<string, string>,
  ) {}

  get url(): string {
    return `http://127.0.0.1:${this.server.port}/${this.token}`;
  }

  /** Every command targets this fixture's own socket, never the user's server. */
  private phux(...args: string[]): void {
    capture([this.binary, ...args, "--socket", this.socketPath], {
      env: this.env,
      timeoutMs: 10_000,
    });
  }

  private act(action: string): void {
    if (action === "open" && !this.opened) {
      this.phux("agent", "session", "open", "default", "--provider", "pi");
      this.opened = true;
      this.phux(
        "agent",
        "emit",
        "default",
        "--type",
        "session_start",
        "--data",
        '{"provider":"pi"}',
      );
    } else if (action === "close" && this.opened) {
      this.phux("agent", "session", "close", "default");
      this.opened = false;
    } else if (["prompt", "ask", "stop"].includes(action) && this.opened) {
      this.phux("agent", "emit", "default", "--type", action);
    } else {
      throw new Failure("invalid fixture action or lifecycle");
    }
  }

  private handle(request: Request): Response {
    if (request.method !== "POST") return new Response("unsupported method", { status: 501 });
    const prefix = `/${this.token}/`;
    const path = new URL(request.url).pathname;
    if (!path.startsWith(prefix)) return new Response("not found", { status: 404 });
    const headers = { "Access-Control-Allow-Origin": "*" };
    try {
      this.act(path.slice(prefix.length));
      return new Response("ok", { headers });
    } catch (error) {
      return new Response(describe(error), { status: 500, headers });
    }
  }

  async close(): Promise<void> {
    await this.server.stop(true);
    if (this.opened) this.act("close");
  }
}
