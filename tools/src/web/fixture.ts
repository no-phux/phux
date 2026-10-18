// Test-only local owner for browser AgentSession lifecycle acceptance.
//
// The browser is not a producer: only the disposable server's Unix owner can
// append records. This loopback fixture offers canned actions, never
// arbitrary commands, behind an unguessable per-run path. It owns no
// production state.
import { randomBytes } from "node:crypto";

export class FixtureError extends Error {}

/** Run one phux CLI command; a non-zero exit is a FixtureError carrying stderr. */
export type Exec = (
  argv: string[],
  env: Record<string, string>,
  timeoutMs: number,
) => Promise<void>;

export const execCommand: Exec = async (argv, env, timeoutMs) => {
  const child = Bun.spawn(argv, {
    env,
    stdin: "ignore",
    stdout: "ignore",
    stderr: "pipe",
    timeout: timeoutMs,
  });
  const [code, stderr] = await Promise.all([child.exited, new Response(child.stderr).text()]);
  if (code !== 0) throw new FixtureError(stderr);
};

const COMMAND_TIMEOUT_MS = 10_000;

export class AgentFixture {
  readonly token = randomBytes(24).toString("base64url");
  opened = false;
  private server: ReturnType<typeof Bun.serve> | undefined;
  // One request at a time, like the single-threaded owner it stands in for.
  private queue: Promise<unknown> = Promise.resolve();

  constructor(
    readonly binary: string,
    readonly socketPath: string,
    readonly env: Record<string, string>,
    private readonly exec: Exec = execCommand,
  ) {}

  get url(): string {
    if (this.server === undefined) throw new Error("AgentFixture is not started");
    return `http://127.0.0.1:${this.server.port}/${this.token}`;
  }

  get serving(): boolean {
    return this.server !== undefined;
  }

  /** Every command targets this fixture's own socket, never the user's server. */
  command(...args: string[]): Promise<void> {
    return this.exec(
      [this.binary, ...args, "--socket", this.socketPath],
      this.env,
      COMMAND_TIMEOUT_MS,
    );
  }

  async action(action: string): Promise<void> {
    if (action === "open" && !this.opened) {
      await this.command("agent", "session", "open", "default", "--provider", "pi");
      this.opened = true;
      await this.command(
        "agent",
        "emit",
        "default",
        "--type",
        "session_start",
        "--data",
        '{"provider":"pi"}',
      );
    } else if (action === "close" && this.opened) {
      await this.command("agent", "session", "close", "default");
      this.opened = false;
    } else if (["prompt", "ask", "stop"].includes(action) && this.opened) {
      await this.command("agent", "emit", "default", "--type", action);
    } else {
      throw new FixtureError("invalid fixture action or lifecycle");
    }
  }

  private async handle(request: Request): Promise<Response> {
    if (request.method !== "POST") return new Response("unsupported method", { status: 501 });
    const prefix = `/${this.token}/`;
    const path = new URL(request.url).pathname;
    if (!path.startsWith(prefix)) return new Response("not found", { status: 404 });
    let status = 200;
    let body = "ok";
    try {
      await this.action(path.slice(prefix.length));
    } catch (error) {
      if (!(error instanceof FixtureError)) throw error;
      [status, body] = [500, error.message];
    }
    return new Response(body, { status, headers: { "Access-Control-Allow-Origin": "*" } });
  }

  start(): this {
    this.server = Bun.serve({
      hostname: "127.0.0.1",
      port: 0,
      fetch: (request) => {
        const response = this.queue.then(() => this.handle(request));
        this.queue = response.catch(() => undefined);
        return response;
      },
    });
    return this;
  }

  async close(): Promise<void> {
    await this.server?.stop(true);
    this.server = undefined;
    if (this.opened) await this.action("close");
  }
}
