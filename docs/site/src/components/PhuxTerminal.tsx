import { useEffect, useId, useRef, useState } from "react";
import {
  mountPhuxTerminal,
  type HostedEvent,
  type PhuxController,
  type SessionBackend,
  type SessionFallbackReason,
} from "./terminal/core";
import { waitForMeaningfulCanvasPaint } from "./terminal/canvas-readiness";
import {
  postEmbedClose,
  postEmbedSession,
  postEmbedStatus,
  type EmbedStatus,
} from "./terminal/embed-status";

interface Props {
  wsUrl?: string;
  cols?: number;
  rows?: number;
  poster?: string;
  posterAlt?: string;
  mode?: "demo" | "portfolio" | "native";
  autoStart?: boolean;
  statusParentOrigin?: string;
}

type Phase =
  | "idle"
  | "checking"
  | "unlock"
  | "connecting"
  | "live"
  | "closed"
  | "error";

interface Identity {
  authenticated: true;
  provider: "github" | "google";
  display: string;
  email?: string;
}

interface SessionView {
  backend: SessionBackend;
  expiresAt: number;
  fallbackReason?: SessionFallbackReason;
}

const fallbackCopy: Record<SessionFallbackReason, string> = {
  "auth-required": "Sign in to unlock native Linux. This session uses the edge shell.",
  "account-concurrency": "Your native shell is already active. This session uses edge.",
  "hourly-quota": "Six native launches used this hour. Edge remains available.",
  "daily-quota": "Thirty native minutes used today. Edge remains available.",
  "native-capacity": "Native capacity is full. You were moved to edge instantly.",
  "ip-capacity": "A native shell is active on this network. This session uses edge.",
  "native-disabled": "Native shells are paused. The edge shell is still online.",
  "native-unhealthy": "Native startup is recovering. The edge shell is online.",
  "startup-timeout": "Native startup timed out. The edge shell took over.",
  "startup-failed": "Native startup failed safely. The edge shell took over.",
};

export default function PhuxTerminal({
  wsUrl = "",
  cols = 100,
  rows = 24,
  poster = "",
  posterAlt = "a phux session",
  mode = "demo",
  autoStart = false,
  statusParentOrigin,
}: Props) {
  const canvasId = `phux-term-${useId().replace(/[:]/g, "")}`;
  const [phase, setPhase] = useState<Phase>(
    mode === "native" ? "checking" : "idle",
  );
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [session, setSession] = useState<SessionView | null>(null);
  const [secondsLeft, setSecondsLeft] = useState<number | null>(null);
  const started = useRef(false);
  const readinessAbort = useRef<AbortController | null>(null);
  const client = useRef<PhuxController | null>(null);
  const popup = useRef<Window | null>(null);
  const authPoll = useRef<number | null>(null);

  function signal(status: EmbedStatus) {
    if (!statusParentOrigin || window.parent === window) return;
    postEmbedStatus(window.parent, document.referrer, statusParentOrigin, status);
  }

  function authOrigin(): string {
    return new URL(wsUrl.replace(/^ws/, "http")).origin;
  }

  async function readIdentity(): Promise<Identity | null> {
    if (!wsUrl) return null;
    const response = await fetch(`${authOrigin()}/auth/session`, {
      credentials: "include",
      headers: { Accept: "application/json" },
    });
    if (!response.ok) return null;
    const value = (await response.json()) as Record<string, unknown>;
    if (
      value.authenticated !== true ||
      (value.provider !== "github" && value.provider !== "google") ||
      typeof value.display !== "string"
    ) {
      return null;
    }
    return value as unknown as Identity;
  }

  function handleEvent(event: HostedEvent) {
    if (event.type === "phux.session.v1") {
      const next = {
        backend: event.backend,
        expiresAt: event.expiresAt,
        ...(event.fallbackReason
          ? { fallbackReason: event.fallbackReason }
          : {}),
      };
      setSession(next);
      if (statusParentOrigin && window.parent !== window) {
        postEmbedSession(
          window.parent,
          document.referrer,
          statusParentOrigin,
          next,
        );
      }
      return;
    }
    if (event.type === "error") {
      setPhase("error");
      signal("error");
      return;
    }
    setPhase("closed");
    signal("closed");
    if (statusParentOrigin && window.parent !== window) {
      postEmbedClose(
        window.parent,
        document.referrer,
        statusParentOrigin,
        event,
      );
    }
  }

  async function launch() {
    if (!wsUrl || started.current) return;
    started.current = true;
    setSession(null);
    setPhase("connecting");
    signal("loading");
    const controller = new AbortController();
    readinessAbort.current = controller;
    try {
      await probe(wsUrl);
      const canvas = document.getElementById(canvasId);
      if (!(canvas instanceof HTMLCanvasElement)) {
        throw new Error("Terminal canvas unavailable");
      }
      const [mounted] = await Promise.all([
        mountPhuxTerminal({
          wsUrl,
          canvasId,
          cols,
          rows,
          mode,
          onEvent: handleEvent,
        }),
        waitForMeaningfulCanvasPaint(canvas, { signal: controller.signal }),
      ]);
      client.current = mounted;
      if (controller.signal.aborted) {
        mounted.close();
        return;
      }
      setPhase("live");
      signal("live");
      if (!autoStart) canvas.focus();
    } catch {
      if (controller.signal.aborted) return;
      controller.abort();
      started.current = false;
      setPhase("error");
      signal("error");
    }
  }

  function release() {
    readinessAbort.current?.abort();
    client.current?.close();
    client.current = null;
    started.current = false;
    setSession(null);
    setSecondsLeft(null);
    setPhase(mode === "native" && !identity ? "unlock" : "idle");
  }

  function beginAuth(provider: "github" | "google") {
    const returnTo = encodeURIComponent("/embed");
    popup.current = window.open(
      `${authOrigin()}/auth/${provider}?returnTo=${returnTo}`,
      "phux-auth",
      "popup,width=620,height=760",
    );
    if (authPoll.current !== null) window.clearInterval(authPoll.current);
    const deadline = Date.now() + 2 * 60_000;
    let checking = false;
    authPoll.current = window.setInterval(async () => {
      if (checking) return;
      if (Date.now() >= deadline) {
        window.clearInterval(authPoll.current ?? undefined);
        authPoll.current = null;
        return;
      }
      checking = true;
      const current = await readIdentity().catch(() => null);
      checking = false;
      if (!current) return;
      window.clearInterval(authPoll.current ?? undefined);
      authPoll.current = null;
      popup.current?.close();
      setIdentity(current);
      setPhase("idle");
      void launch();
    }, 750);
  }

  async function logout() {
    await fetch(`${authOrigin()}/auth/logout`, {
      method: "POST",
      credentials: "include",
    });
    release();
    setIdentity(null);
    setPhase("unlock");
  }

  useEffect(() => {
    let active = true;
    async function initialize() {
      if (!wsUrl) {
        setPhase("idle");
        return;
      }
      if (mode !== "native") {
        if (autoStart) void launch();
        return;
      }
      const current = await readIdentity().catch(() => null);
      if (!active) return;
      setIdentity(current);
      if (current && autoStart) void launch();
      else {
        setPhase(current ? "idle" : "unlock");
        if (!current) signal("unlock");
      }
    }
    void initialize();

    async function authComplete(event: MessageEvent) {
      if (
        event.origin !== window.location.origin ||
        !event.data ||
        typeof event.data !== "object" ||
        event.data.source !== "phux-auth" ||
        event.data.type !== "complete"
      ) {
        return;
      }
      const current = await readIdentity().catch(() => null);
      if (!active || !current) return;
      if (authPoll.current !== null) window.clearInterval(authPoll.current);
      authPoll.current = null;
      popup.current?.close();
      setIdentity(current);
      setPhase("idle");
      void launch();
    }
    window.addEventListener("message", authComplete);
    return () => {
      active = false;
      window.removeEventListener("message", authComplete);
      readinessAbort.current?.abort();
      client.current?.close();
      if (authPoll.current !== null) window.clearInterval(authPoll.current);
      popup.current?.close();
    };
  }, []);

  useEffect(() => {
    if (!session) return;
    const update = () =>
      setSecondsLeft(Math.max(0, Math.ceil((session.expiresAt - Date.now()) / 1000)));
    update();
    const timer = window.setInterval(update, 1_000);
    return () => window.clearInterval(timer);
  }, [session]);

  const native = mode === "native";
  const fallback = session?.fallbackReason;
  return (
    <div className={`pterm${native ? " pterm-native" : ""}`} data-status={phase}>
      {native && (
        <header className="pterm-head">
          <span className="pterm-mark">PHUX / HOSTED</span>
          <span className="pterm-state">{phase}</span>
          <span className="pterm-head-spacer" />
          {session && <b>{session.backend.toUpperCase()}</b>}
          {secondsLeft !== null && <span>{formatTime(secondsLeft)}</span>}
        </header>
      )}
      <div className="pterm-screen">
        {poster && (
          <img
            className="pterm-poster"
            src={poster}
            width={cols * 8}
            height={rows * 16}
            alt={posterAlt}
            decoding="async"
          />
        )}
        <canvas
          id={canvasId}
          className={`pterm-canvas${poster ? " is-overlay" : ""}`}
          width={cols * 8}
          height={rows * 16}
          tabIndex={0}
          aria-label={
            native
              ? "real disposable Linux shell over phux"
              : mode === "portfolio"
                ? "interactive read-only GitHub portfolio"
                : "live phux terminal"
          }
        />
        {phase !== "live" && (
          <div className="pterm-overlay">
            {phase === "checking" && <p className="pterm-label">checking access…</p>}
            {phase === "unlock" && (
              <div className="pterm-unlock">
                <span className="pterm-kicker">NATIVE ACCESS</span>
                <h2>Unlock a real Linux shell.</h2>
                <p>Five disposable minutes. Full toolchain. No network, credentials, or persistence.</p>
                <div className="pterm-actions">
                  <button onClick={() => beginAuth("github")}>Continue with GitHub</button>
                  <button onClick={() => beginAuth("google")}>Continue with Google</button>
                  <button className="is-quiet" onClick={launch}>Use instant edge shell</button>
                </div>
              </div>
            )}
            {phase === "idle" &&
              (wsUrl ? (
                <button className="pterm-launch" onClick={launch}>
                  ▶ {native ? "launch native shell" : "launch the live demo"}
                </button>
              ) : (
                <span className="pterm-dim pterm-label">live demo coming online</span>
              ))}
            {phase === "connecting" && (
              <span className="pterm-dim pterm-label" role="status">selecting backend…</span>
            )}
            {(phase === "closed" || phase === "error") && (
              <div className="pterm-retry" role="status">
                <p>{phase === "closed" ? "Session ended cleanly." : "The session could not start."}</p>
                <button className="pterm-launch" onClick={release}>return to controls</button>
              </div>
            )}
          </div>
        )}
      </div>
      {native && (
        <>
          <nav className="pterm-controls" aria-label="Shell controls">
            <span>
              {identity
                ? `${identity.provider} / ${identity.display}`
                : "anonymous / edge eligible"}
            </span>
            <span className="pterm-control-spacer" />
            {(phase === "live" || phase === "connecting") && (
              <button onClick={release}>release session</button>
            )}
            {identity && <button onClick={() => void logout()}>sign out</button>}
          </nav>
          <aside className="pterm-facts" aria-live="polite">
            <span><b>runtime</b> ephemeral</span>
            <span><b>egress</b> blocked</span>
            <span><b>storage</b> none</span>
            {fallback && <p>{fallbackCopy[fallback]}</p>}
          </aside>
        </>
      )}
    </div>
  );
}

function formatTime(seconds: number): string {
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${String(seconds % 60).padStart(2, "0")}`;
}

function probe(wsUrl: string): Promise<unknown> {
  const health = new URL(wsUrl.replace(/^ws/, "http"));
  health.pathname = "/healthz";
  return fetch(health, { mode: "no-cors", signal: AbortSignal.timeout(4_000) });
}
