import { Container } from "@cloudflare/containers";
import type { StopParams } from "@cloudflare/containers";
import type { GlobalCapDO } from "./global-cap";
import { serializeSessionInfo } from "./session-info";
import { relaySessionSockets } from "./native-start";

interface Env {
  GLOBAL_CAP: DurableObjectNamespace<GlobalCapDO>;
}

interface SessionState {
  sid: string;
  released: boolean;
  expiresAt: number;
}

const STATE_KEY = "native-session";

export class PhuxSessionContainer extends Container<Env> {
  defaultPort = 8080;
  requiredPorts = [8082];
  enableInternet = false;
  sleepAfter = "15s";
  private releaseInFlight: Promise<void> | undefined;
  private browserSockets = new Set<WebSocket>();

  async prepare(
    sid: string,
    expiresAt: number,
    startupTimeoutMs: number,
  ): Promise<void> {
    if (
      !sid ||
      !Number.isFinite(expiresAt) ||
      expiresAt <= Date.now() ||
      !Number.isFinite(startupTimeoutMs) ||
      startupTimeoutMs <= 0
    ) {
      throw new Error("invalid native session preparation");
    }

    const state = await this.ctx.storage.get<SessionState>(STATE_KEY);
    if (state && state.sid !== sid) {
      throw new Error("native container is already bound to another session");
    }
    expiresAt = state?.expiresAt ?? expiresAt;
    if (!state)
      await this.ctx.storage.put<SessionState>(STATE_KEY, {
        sid,
        released: false,
        expiresAt,
      });

    // A retried prepare keeps the original wall-clock deadline.
    await this.schedule(new Date(expiresAt), "hardExpire", sid);
    await this.startAndWaitForPorts({
      ports: 8082,
      cancellationOptions: {
        abort: AbortSignal.timeout(startupTimeoutMs),
        instanceGetTimeoutMS: startupTimeoutMs,
        portReadyTimeoutMS: startupTimeoutMs,
        waitInterval: 100,
      },
    });
  }

  async abortStartup(): Promise<void> {
    await this.destroy();
  }

  override async fetch(request: Request): Promise<Response> {
    const response = await super.fetch(request);
    if (response.status !== 101 || !response.webSocket) return response;

    const state = await this.ctx.storage.get<SessionState>(STATE_KEY);
    if (!state || state.expiresAt <= Date.now()) {
      try {
        response.webSocket.close(1011, "native session unavailable");
      } catch {
        // The upstream may already have closed.
      }
      return new Response("native session unavailable", { status: 503 });
    }

    const upstream = response.webSocket;
    upstream.accept();
    const pair = new WebSocketPair();
    const [client, browser] = Object.values(pair);
    browser.accept();
    this.browserSockets.add(browser);
    relaySessionSockets(
      browser,
      upstream,
      serializeSessionInfo({
        type: "phux.session.v1",
        outcome: "accepted",
        backend: "native",
        expiresAt: state.expiresAt,
      }),
      () => this.browserSockets.delete(browser),
    );
    return new Response(null, { status: 101, webSocket: client });
  }

  async hardExpire(sid: string): Promise<void> {
    const state = await this.ctx.storage.get<SessionState>(STATE_KEY);
    if (!state || state.sid !== sid) return;
    for (const socket of this.browserSockets) {
      try {
        socket.close(4005, "closed: session time limit");
      } catch {
        // Already closed.
      }
    }
    this.browserSockets.clear();
    await this.releaseAndDestroy();
  }

  override async onActivityExpired(): Promise<void> {
    await this.releaseAndDestroy();
  }

  override onStart(): void {
    console.log("native_container_start health_port=8082 websocket_port=8080");
  }

  override async onStop(params: StopParams): Promise<void> {
    console.log(
      `native_container_stop exit_code=${params.exitCode} reason=${params.reason}`,
    );
    await this.releaseReservation();
  }

  override onError(): void {
    console.error("native_container_error phase=start_or_port_check");
  }

  private async releaseAndDestroy(): Promise<void> {
    try {
      await this.releaseReservation();
    } finally {
      await this.destroy();
    }
  }

  private releaseReservation(): Promise<void> {
    this.releaseInFlight ??= this.releaseReservationOnce().finally(() => {
      this.releaseInFlight = undefined;
    });
    return this.releaseInFlight;
  }

  private async releaseReservationOnce(): Promise<void> {
    const state = await this.ctx.storage.get<SessionState>(STATE_KEY);
    if (!state || state.released) return;

    await this.env.GLOBAL_CAP.getByName("global").releaseNative(state.sid);
    await this.ctx.storage.put<SessionState>(STATE_KEY, { ...state, released: true });
  }
}
