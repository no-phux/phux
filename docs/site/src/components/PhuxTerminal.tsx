import { useEffect, useId, useRef, useState } from "react";
import {
  mountPhuxTerminal,
  terminalGeometry,
  warmPhuxTerminal,
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
import { nativeAuthStartUrl } from "./terminal/auth-start";
import { SITE } from "../lib/site";

interface Props {
  wsUrl?: string;
  cols?: number;
  rows?: number;
  poster?: string;
  posterAlt?: string;
  mode?: "demo" | "portfolio" | "native";
  autoStart?: boolean;
  focusOnStart?: boolean;
  statusParentOrigin?: string;
}

type Phase =
  "idle" | "checking" | "unlock" | "connecting" | "live" | "closed" | "error";

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

interface Attempt {
  abort: AbortController;
  client?: PhuxController;
}

const fallbackCopy: Record<SessionFallbackReason, string> = {
  "auth-required":
    "Sign in to unlock native Linux. This session uses the edge shell.",
  "account-concurrency":
    "Your native shell is already active. This session uses edge.",
  "hourly-quota": "Six native launches used this hour. Edge remains available.",
  "daily-quota": "Thirty native minutes used today. Edge remains available.",
  "native-capacity":
    "Native capacity is full. This session uses the edge shell.",
  "ip-capacity":
    "A native shell is active on this network. This session uses edge.",
  "native-disabled":
    "Native shells are paused. The edge shell is still online.",
  "native-unhealthy": "Native startup is recovering. The edge shell is online.",
  "startup-timeout": "Native startup timed out. The edge shell took over.",
  "startup-failed": "Native startup failed safely. The edge shell took over.",
};

const closeCopy: Record<
  Extract<HostedEvent, { type: "close" }>["category"],
  string
> = {
  normal: "The shell exited. Launch again for a fresh session.",
  "going-away": "The server is restarting. Try a fresh session shortly.",
  protocol:
    "The terminal connection was not compatible. Reload the page and try again.",
  server: "The shell stopped unexpectedly. Try a fresh session.",
  unavailable:
    "Hosted shells are temporarily unavailable. Please try again shortly.",
  capacity:
    "All hosted slots are busy. Release any other demo sessions, or try again shortly.",
  "rate-limited":
    "Too many launches in a short time. Wait a little before trying again.",
  "bad-request":
    "The server could not accept this session. Reload the page and try again.",
  idle: "This idle shell was released automatically. Launch again when you are ready.",
  expired:
    "This disposable session reached its time limit. Launch again for a fresh shell.",
  unauthorized:
    "Native access expired. Sign in again, or choose the no-sign-in edge shell.",
  network:
    "The connection was lost. Check your network and try a fresh session.",
};

const errorCopy: Record<
  Extract<HostedEvent, { type: "error" }>["category"],
  string
> = {
  protocol:
    "The terminal received an incompatible response. Reload the page and try again.",
  client:
    "The terminal could not read the server response. Reload the page and try again.",
  transport:
    "The connection failed. Check your network; hosted capacity may also be temporarily unavailable.",
};

export default function PhuxTerminal({
  wsUrl = "",
  cols = 100,
  rows = 24,
  poster = "",
  posterAlt = `${SITE.name} session`,
  mode = "demo",
  autoStart = false,
  focusOnStart = false,
  statusParentOrigin,
}: Props) {
  const canvasPrefix = `phux-term-${useId().replace(/[:]/g, "")}`;
  const canvasHost = useRef<HTMLDivElement | null>(null);
  const sequence = useRef(0);
  const attempt = useRef<Attempt | null>(null);
  const [phase, setPhase] = useState<Phase>(
    mode === "native" ? "checking" : "idle",
  );
  const [identity, setIdentity] = useState<Identity | null>(null);
  const [session, setSession] = useState<SessionView | null>(null);
  const [secondsLeft, setSecondsLeft] = useState<number | null>(null);
  const [startupMs, setStartupMs] = useState<number | null>(null);
  const [message, setMessage] = useState("");

  function signal(status: EmbedStatus) {
    if (!statusParentOrigin || window.parent === window) return;
    postEmbedStatus(
      window.parent,
      document.referrer,
      statusParentOrigin,
      status,
    );
  }

  function stop() {
    const current = attempt.current;
    attempt.current = null;
    current?.abort.abort();
    current?.client?.close();
  }

  async function readIdentity(signal: AbortSignal): Promise<Identity | null> {
    const response = await fetch(`${window.location.origin}/auth/session`, {
      credentials: "include",
      headers: { Accept: "application/json" },
      signal,
    });
    if (!response.ok) return null;
    const value = (await response.json()) as Record<string, unknown>;
    if (
      value.authenticated !== true ||
      (value.provider !== "github" && value.provider !== "google") ||
      typeof value.display !== "string"
    )
      return null;
    return value as unknown as Identity;
  }

  function handleEvent(event: HostedEvent) {
    if (event.type === "phux.session.v1") {
      const next: SessionView = {
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
    stop();
    setSession(null);
    setSecondsLeft(null);
    setPhase(event.type === "error" ? "error" : "closed");
    signal(event.type === "error" ? "error" : "closed");
    if (event.type === "error") {
      setMessage(errorCopy[event.category]);
      return;
    }
    setMessage(closeCopy[event.category]);
    if (statusParentOrigin && window.parent !== window) {
      postEmbedClose(
        window.parent,
        document.referrer,
        statusParentOrigin,
        event,
      );
    }
  }

  function createCanvas() {
    const host = canvasHost.current;
    if (!host) throw new Error("Terminal canvas unavailable");
    const geometry = terminalGeometry(
      host.clientWidth,
      window.innerHeight * 0.55,
      cols,
      rows,
    );
    const canvas = document.createElement("canvas");
    canvas.id = `${canvasPrefix}-${++sequence.current}`;
    canvas.className = "pterm-canvas";
    canvas.width = geometry.cols * 8;
    canvas.height = geometry.rows * 16;
    canvas.tabIndex = 0;
    canvas.setAttribute(
      "aria-label",
      "Live hosted terminal. Click to focus and type commands.",
    );
    // A distinct canvas prevents a late, cancelled mount from painting into
    // the next attempt or satisfying its readiness check with old pixels.
    host.replaceChildren(canvas);
    return { canvas, geometry };
  }

  async function launch(selectedMode = mode) {
    if (!wsUrl || attempt.current) return;
    const current: Attempt = { abort: new AbortController() };
    attempt.current = current;
    const isCurrent = () =>
      attempt.current === current && !current.abort.signal.aborted;
    const began = performance.now();
    let accepted = false;
    setSession(null);
    setSecondsLeft(null);
    setStartupMs(null);
    setMessage("");
    setPhase("connecting");
    signal("loading");
    try {
      // Let a newly opened dialog acquire its layout before choosing the grid.
      await new Promise<void>((resolve) =>
        requestAnimationFrame(() => resolve()),
      );
      if (!isCurrent()) return;
      const { canvas, geometry } = createCanvas();
      await Promise.all([
        mountPhuxTerminal({
          wsUrl,
          canvasId: canvas.id,
          ...geometry,
          mode: selectedMode,
          signal: current.abort.signal,
          onEvent(event) {
            if (!isCurrent()) return;
            if (event.type === "phux.session.v1") accepted = true;
            handleEvent(event);
          },
        }).then((mounted) => {
          if (!isCurrent()) mounted.close();
          else current.client = mounted;
        }),
        waitForMeaningfulCanvasPaint(canvas, { signal: current.abort.signal }),
      ]);
      if (!isCurrent()) return;
      if (!accepted)
        throw new Error("The server did not accept the hosted session.");
      setStartupMs(performance.now() - began);
      setPhase("live");
      signal("live");
    } catch {
      if (!isCurrent()) return;
      stop();
      setSession(null);
      setSecondsLeft(null);
      setMessage(
        "The shell did not become ready. Check your connection and try again; any allocated session has been released.",
      );
      setPhase("error");
      signal("error");
    }
  }

  function release() {
    stop();
    canvasHost.current?.replaceChildren();
    setSession(null);
    setSecondsLeft(null);
    setStartupMs(null);
    setPhase(mode === "native" && !identity ? "unlock" : "idle");
    signal("closed");
  }

  function beginAuth(provider: "github" | "google") {
    const returnPath = window.location.pathname === "/embed" ? "/embed" : "/";
    window.location.assign(
      nativeAuthStartUrl(window.location.origin, provider, returnPath),
    );
  }

  async function logout() {
    release();
    try {
      const response = await fetch(`${window.location.origin}/auth/logout`, {
        method: "POST",
        credentials: "include",
      });
      if (!response.ok) throw new Error("Sign out failed");
      setIdentity(null);
      setPhase("unlock");
    } catch {
      setMessage(
        "Sign out did not complete. Please check your connection and try again.",
      );
      setPhase("error");
    }
  }

  useEffect(() => {
    const auth = new AbortController();
    async function initialize() {
      if (!wsUrl) {
        setPhase("idle");
        return;
      }
      if (mode !== "native") {
        if (autoStart) void launch();
        return;
      }
      const current = await readIdentity(auth.signal).catch(() => null);
      if (auth.signal.aborted) return;
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
        mode !== "native" ||
        event.origin !== window.location.origin ||
        !event.data ||
        typeof event.data !== "object" ||
        event.data.source !== "phux-auth" ||
        event.data.type !== "complete"
      )
        return;
      const current = await readIdentity(auth.signal).catch(() => null);
      if (auth.signal.aborted || !current) return;
      setIdentity(current);
      if (!attempt.current) void launch("native");
    }
    window.addEventListener("message", authComplete);
    return () => {
      auth.abort();
      window.removeEventListener("message", authComplete);
      stop();
    };
  }, []);

  useEffect(() => {
    // Focus only after React has committed the visible live canvas.
    if (phase === "live" && (!autoStart || focusOnStart)) {
      canvasHost.current?.querySelector("canvas")?.focus();
    }
  }, [phase, autoStart, focusOnStart]);

  useEffect(() => {
    if (!session) return;
    const update = () =>
      setSecondsLeft(
        Math.max(0, Math.ceil((session.expiresAt - Date.now()) / 1000)),
      );
    update();
    const timer = window.setInterval(update, 1_000);
    return () => window.clearInterval(timer);
  }, [session]);

  const native = mode === "native";
  const fallback = session?.fallbackReason;
  const warm = () => {
    void warmPhuxTerminal().catch(() => {});
  };
  const running = phase === "live" || phase === "connecting";
  return (
    <div
      className={`pterm${native ? " pterm-native" : ""}`}
      data-status={phase}
    >
      <header className="pterm-head">
        <span className="pterm-mark">{SITE.name} / hosted</span>
        <span className="pterm-state" aria-live="polite">
          {phase}
        </span>
        <span className="pterm-head-spacer" />
        {session && (
          <b>{session.backend === "native" ? "Linux" : "edge WASM"}</b>
        )}
        {secondsLeft !== null && (
          <span title="Session time remaining">{formatTime(secondsLeft)}</span>
        )}
      </header>
      <div className="pterm-screen">
        {poster && !running && (
          <img
            className="pterm-poster"
            src={poster}
            width={cols * 8}
            height={rows * 16}
            alt={posterAlt}
            decoding="async"
          />
        )}
        <div ref={canvasHost} className="pterm-canvas-host" />
        {phase !== "live" && (
          <div className="pterm-overlay">
            {phase === "checking" && (
              <p className="pterm-label" role="status">
                checking native access…
              </p>
            )}
            {phase === "unlock" && (
              <div className="pterm-unlock">
                <span className="pterm-kicker">NATIVE ACCESS</span>
                <h2>Open a real Linux shell.</h2>
                <p>
                  Five disposable minutes. No outbound network or persistence.
                  Or try the smaller edge WASM shell without signing in.
                </p>
                <div className="pterm-actions">
                  <button onClick={() => beginAuth("github")}>
                    Continue with GitHub
                  </button>
                  <button onClick={() => beginAuth("google")}>
                    Continue with Google
                  </button>
                  <button
                    className="is-quiet"
                    onPointerEnter={warm}
                    onFocus={warm}
                    onClick={() => void launch("demo")}
                  >
                    Use instant edge shell
                  </button>
                </div>
              </div>
            )}
            {phase === "idle" &&
              (wsUrl ? (
                <div className="pterm-actions">
                  <button
                    className="pterm-launch"
                    onPointerEnter={warm}
                    onFocus={warm}
                    onClick={() => void launch()}
                  >
                    {native
                      ? "Launch native Linux shell"
                      : "Launch edge WASM shell"}
                  </button>
                  {native && (
                    <button
                      className="is-quiet"
                      onPointerEnter={warm}
                      onFocus={warm}
                      onClick={() => void launch("demo")}
                    >
                      Use instant edge shell
                    </button>
                  )}
                </div>
              ) : (
                <span className="pterm-dim pterm-label">
                  Live demo is not configured.
                </span>
              ))}
            {phase === "connecting" && (
              <span className="pterm-dim pterm-label" role="status">
                Connecting and waiting for the first terminal frame…
              </span>
            )}
            {(phase === "closed" || phase === "error") && (
              <div className="pterm-retry" role="status">
                <p>{message}</p>
                <button className="pterm-launch" onClick={release}>
                  Return to controls
                </button>
              </div>
            )}
          </div>
        )}
      </div>
      <nav className="pterm-controls" aria-label="Shell controls">
        <span>
          {native && identity
            ? `${identity.provider} / ${identity.display}`
            : "no sign-in / disposable"}
        </span>
        {startupMs !== null && phase === "live" && (
          <span title="Measured in this browser from launch to accepted session and visible terminal output">
            Ready in {(startupMs / 1_000).toFixed(1)}s
          </span>
        )}
        <span className="pterm-control-spacer" />
        {running && (
          <button onClick={release}>
            {phase === "connecting" ? "Cancel launch" : "Release session"}
          </button>
        )}
        {identity && <button onClick={() => void logout()}>Sign out</button>}
      </nav>
      <aside className="pterm-facts" aria-live="polite">
        <span>
          <b>runtime</b>{" "}
          {session
            ? session.backend === "native"
              ? "Linux"
              : "edge WASM"
            : native
              ? "Linux requested"
              : "edge WASM"}
        </span>
        <span>
          <b>egress</b> blocked
        </span>
        <span>
          <b>storage</b> temporary
        </span>
        {fallback && <p>{fallbackCopy[fallback]}</p>}
        {!native && (
          <p>
            Real shell output. Limited edge commands; not the native Linux
            toolchain.
          </p>
        )}
      </aside>
      {session?.backend === "native" && phase === "live" && (
        <aside className="pterm-shortcuts" aria-label={`${SITE.name} TUI shortcuts`}>
          <b>
            Run <code>phux</code>, then
          </b>
          <span>
            <kbd>C-a</kbd> <kbd>%</kbd> split left/right
          </span>
          <span>
            <kbd>C-a</kbd> <kbd>&quot;</kbd> split top/bottom
          </span>
          <span>
            <kbd>C-a</kbd> <kbd>c</kbd> new window
          </span>
          <span>
            <kbd>C-a</kbd> <kbd>?</kbd> all keys
          </span>
        </aside>
      )}
    </div>
  );
}

function formatTime(seconds: number): string {
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${String(seconds % 60).padStart(2, "0")}`;
}
