import type { ReactNode } from "react";
import { SITE } from "../lib/site";
import type { SessionBackend } from "./terminal/core";

export type TerminalPhase = "idle" | "checking" | "unlock" | "connecting" | "live" | "closed" | "error";
const phaseLabels: Record<TerminalPhase, string> = {
  idle: "Ready", checking: "Checking access…", unlock: "Sign in",
  connecting: "Connecting…", live: "Connected", closed: "Session ended", error: "Connection issue",
};
type PaneView = { count: number; focused: string; pending: boolean };
type IdentityView = { provider: string; display: string };

export function TerminalHeader({ phase, backend, timeLeft }: {
  phase: TerminalPhase; backend?: SessionBackend; timeLeft: string | null;
}) {
  return <header className="pterm-head">
    <span className="pterm-mark">{SITE.name}</span>
    <span className="pterm-state" aria-live="polite">{phaseLabels[phase]}</span>
    <span className="pterm-head-spacer" />
    {backend && <span>{backend === "native" ? "Linux" : "Demo"}</span>}
    {timeLeft !== null && <span title="Session time remaining">{timeLeft}</span>}
  </header>;
}

type LaunchProps = { native: boolean; warm: () => void; launch: (mode?: "demo" | "native") => void };
function LaunchControls({ native, warm, launch }: LaunchProps) {
  return <div className="pterm-actions">
    <button className="pterm-launch" onPointerEnter={warm} onFocus={warm} onClick={() => launch()}>
      {native ? "Launch native Linux shell" : "Launch edge WASM shell"}
    </button>
    {native && <button className="is-quiet" onPointerEnter={warm} onFocus={warm} onClick={() => launch("demo")}>
      Use instant edge shell
    </button>}
  </div>;
}

function UnlockPanel({ authMessage, warm, launch, beginAuth }: LaunchProps & {
  authMessage: string; beginAuth: (provider: "github" | "google") => void;
}) {
  return <div className="pterm-unlock">
    <h2>A Linux shell, right here.</h2>
    <p>Sign in for a disposable five-minute session. No outbound network or saved files.</p>
    {authMessage && <p className="pterm-auth-error" role="alert">{authMessage}</p>}
    <div className="pterm-actions">
      <button onClick={() => beginAuth("github")}>Continue with GitHub</button>
      <button onClick={() => beginAuth("google")}>Continue with Google</button>
      <button className="is-quiet" onPointerEnter={warm} onFocus={warm} onClick={() => launch("demo")}>Use instant edge shell</button>
    </div>
    <a className="pterm-standalone" href="/embed" target="_blank" rel="noopener noreferrer">Open standalone shell</a>
  </div>;
}

function RetryPanel({ message, release }: { message: string; release: () => void }) {
  return <div className="pterm-retry" role="status">
    <p>{message}</p>
    <button className="pterm-launch" onClick={release}>Return to controls</button>
  </div>;
}

export function TerminalOverlay(props: LaunchProps & {
  phase: TerminalPhase; configured: boolean; authMessage: string; message: string;
  beginAuth: (provider: "github" | "google") => void; release: () => void;
}) {
  if (props.phase === "live") return null;
  const retry = <RetryPanel message={props.message} release={props.release} />;
  const panels: Partial<Record<TerminalPhase, ReactNode>> = {
    checking: <p className="pterm-label" role="status">Checking access…</p>,
    unlock: <UnlockPanel {...props} />,
    idle: props.configured ? <LaunchControls {...props} /> : <p className="pterm-label">Live demo is not configured.</p>,
    connecting: <p className="pterm-label" role="status">Opening your terminal…</p>,
    closed: retry, error: retry,
  };
  return <div className="pterm-overlay" key={props.phase}>{panels[props.phase]}</div>;
}

export function PaneControls({ panes, error, splitVertical, splitHorizontal, nextPane, closePane }: {
  panes: PaneView; error: string; splitVertical: () => void; splitHorizontal: () => void;
  nextPane: () => void; closePane: () => void;
}) {
  return <nav className="pterm-pane-controls" aria-label="Pane controls">
    <span>{panes.count} / 4 panes</span>
    <button aria-label="Split left/right" title="Split left/right" disabled={panes.pending || panes.count >= 4} onClick={splitVertical}>Split</button>
    <button aria-label="Split top/bottom" title="Split top/bottom" disabled={panes.pending || panes.count >= 4} onClick={splitHorizontal}>Stack</button>
    <button disabled={panes.pending || panes.count < 2} onClick={nextPane}>Next pane</button>
    <button disabled={panes.pending || panes.count < 2} onClick={closePane}>Close pane</button>
    <span className="pterm-pane-status" role="status">{error || (panes.pending ? "Updating panes…" : "")}</span>
  </nav>;
}

export function SessionControls({ phase, identity, release, logout }: {
  phase: TerminalPhase; identity: IdentityView | null; release: () => void; logout: () => void;
}) {
  const running = phase === "live" || phase === "connecting";
  return <nav className="pterm-controls" aria-label="Shell controls">
    <span>Disposable session · no network</span>
    <span className="pterm-control-spacer" />
    {running && <button onClick={release}>{phase === "connecting" ? "Cancel launch" : "Release session"}</button>}
    {identity && <button onClick={logout}>Sign out</button>}
  </nav>;
}

export function TerminalInfo({ native, backend, identity, startupMs, live, focused }: {
  native: boolean; backend?: SessionBackend; identity: IdentityView | null;
  startupMs: number | null; live: boolean; focused: string;
}) {
  const runtime = backend ?? (native ? "native" : "edge");
  return <details className="pterm-info">
    <summary>Info &amp; shortcuts</summary>
    <div className="pterm-info-body">
      <aside className="pterm-facts" aria-label="Session details">
        <span><b>Runtime</b> {runtime === "native" ? "Linux" : "Edge demo"}</span>
        <span><b>Network</b> blocked</span>
        <span><b>Files</b> temporary</span>
        {identity && <span><b>Signed in</b> {identity.display} ({identity.provider})</span>}
        {startupMs !== null && live && <span title="Measured in this browser from launch to accepted session and visible terminal output"><b>Ready in</b> {(startupMs / 1_000).toFixed(1)}s</span>}
        {live && focused && <span><b>Focused terminal</b> {focused}</span>}
      </aside>
      {runtime === "edge" && <p className="pterm-info-note">Limited demo commands, not a native Linux toolchain.</p>}
      <aside className="pterm-shortcuts" aria-label="Terminal pane shortcuts">
        <span><kbd>Ctrl+a</kbd> <kbd>%</kbd> split left/right</span>
        <span><kbd>Ctrl+a</kbd> <kbd>&quot;</kbd> split top/bottom</span>
        <span><kbd>Ctrl+a</kbd> <kbd>o</kbd> next pane</span>
        <span><kbd>Ctrl+a</kbd> <kbd>x</kbd> close pane</span>
      </aside>
      <p className="pterm-info-note">Click a pane to type there. Each pane has its own terminal. Closing this demo releases its session; install phux to keep your own work running.</p>
    </div>
  </details>;
}
