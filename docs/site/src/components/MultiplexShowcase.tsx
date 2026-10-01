import { useEffect, useId, useRef, useState } from "react";
import type PhuxTerminal from "./PhuxTerminal";
import { SITE, docsHref } from "../lib/site";
import "./MultiplexShowcase.css";

type View = "split" | "mirror" | "detached";
type Mode = "demo" | "native";
const explanations: Record<View, { title: string; detail: string }> = {
  split: {
    title: "Independent terminals",
    detail:
      "Each terminal has its own process and identity. Panes arrange their views.",
  },
  mirror: {
    title: "Multiple clients",
    detail:
      "The desktop and browser attach to terminal 01. Both receive its output and can send input.",
  },
  detached: {
    title: "Detach and reconnect",
    detail:
      "Disconnecting leaves the terminal running on the server. Reattach to resume.",
  },
};

export default function MultiplexShowcase({ wsUrl = "" }: { wsUrl?: string }) {
  const [view, setView] = useState<View>("split");
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<Mode>("demo");
  const [Terminal, setTerminal] = useState<typeof PhuxTerminal | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [authError, setAuthError] = useState("");
  const dialog = useRef<HTMLDialogElement>(null);
  const opener = useRef<HTMLElement | null>(null);
  const loading = useRef<Promise<void> | null>(null);
  const id = useId();

  function loadTerminal() {
    if (!loading.current) {
      setLoadFailed(false);
      loading.current = import("./PhuxTerminal")
        .then((module) => setTerminal(() => module.default))
        .catch(() => {
          loading.current = null;
          setLoadFailed(true);
        });
    }
  }

  function launch() {
    opener.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    setMode("demo");
    setOpen(true);
    loadTerminal();
  }

  useEffect(() => {
    const result = document.documentElement.dataset.nativeAuthResult;
    if (!result) return;
    delete document.documentElement.dataset.nativeAuthResult;
    setAuthError(
      result === "error"
        ? (document.documentElement.dataset.nativeAuthError ??
            "Sign-in did not finish. Try a provider again below, or choose the edge tour.")
        : "",
    );
    delete document.documentElement.dataset.nativeAuthError;
    setMode("native");
    setOpen(true);
    loadTerminal();
  }, []);

  useEffect(() => {
    if (!open) return;
    const element = dialog.current;
    if (!element) return;
    element.showModal();
    const overflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      element.close();
      document.body.style.overflow = overflow;
      opener.current?.focus({ preventScroll: true });
    };
  }, [open]);

  const explanation = explanations[view];
  return (
    <div className="mux-showcase" id="demo">
      <div className="mux-window" aria-label="Interactive multiplexing diagram">
        <header className="mux-titlebar">
          <span className="mux-brand">
            {SITE.name}<span aria-hidden="true"> / </span>
            <span>terminal model</span>
          </span>
          <span className="mux-diagram-label">interactive diagram</span>
        </header>
        <div
          className="mux-tabs"
          role="group"
          aria-label="Explore multiplexing"
        >
          <button
            type="button"
            aria-pressed={view === "split"}
            onClick={() => setView("split")}
          >
            <span aria-hidden="true">01</span> Split
          </button>
          <button
            type="button"
            aria-pressed={view === "mirror"}
            onClick={() => setView("mirror")}
          >
            <span aria-hidden="true">02</span> Share a view
          </button>
          <button
            type="button"
            aria-pressed={view === "detached"}
            onClick={() => setView("detached")}
          >
            <span aria-hidden="true">03</span> Detach
          </button>
        </div>
        <div className="mux-views" data-view={view}>
          <TerminalView label="your desktop" />
          <div className="mux-pane-secondary" key={view}>
            {view === "detached" ? (
              <div className="mux-pane mux-detached">
                <span className="mux-detached-icon" aria-hidden="true">
                  ↗
                </span>
                <b>Browser detached</b>
                <span>Terminal 01 keeps running.</span>
                <button type="button" onClick={() => setView("mirror")}>
                  Reattach <span aria-hidden="true">↵</span>
                </button>
              </div>
            ) : (
              <TerminalView
                label={view === "mirror" ? "your browser" : "another pane"}
                separate={view === "split"}
              />
            )}
          </div>
        </div>
        <div className="mux-wire" aria-hidden="true">
          <span className="mux-wire-line" />
          <span>public protocol</span>
          <span className="mux-wire-line" />
        </div>
        <div className="mux-server">
          <span className="mux-server-icon" aria-hidden="true">
            ▤
          </span>
          <div>
            <b>{SITE.name} server</b>
            <span>Owns the terminals and their processes.</span>
          </div>
          <span className="mux-resource">01 + 02</span>
        </div>
        <div
          className="mux-explanation"
          role="status"
          aria-live="polite"
          aria-atomic="true"
        >
          <b>{explanation.title}</b>
          <p>{explanation.detail}</p>
        </div>
        <div className="mux-launch-row">
          <button
            type="button"
            className="mux-launch"
            onClick={launch}
            aria-haspopup="dialog"
          >
            <span className="mux-launch-symbol" aria-hidden="true">
              &gt;_
            </span>
            <span>
              Try a terminal
              <small>Edge tour · no install or sign-in</small>
            </span>
            <span aria-hidden="true">↗</span>
          </button>
        </div>
      </div>
      <p className="mux-caption">
        The diagram shows the model. The demo opens a disposable terminal.
      </p>
      <noscript>
        <p>
          JavaScript is required for the interactive demo.{" "}
          <a href="/embed">Open the hosted shell</a> or{" "}
          <a href={docsHref("/quickstart")}>start locally</a>.
        </p>
      </noscript>
      <dialog
        ref={dialog}
        className="mux-dialog"
        aria-labelledby={`${id}-title`}
        aria-describedby={`${id}-description`}
        onCancel={(event) => {
          event.preventDefault();
          setOpen(false);
        }}
        onClose={() => setOpen(false)}
      >
        <header className="mux-dialog-head">
          <div>
            <span className="mux-eyebrow">HOSTED DEMO</span>
            <h2 id={`${id}-title`}>{SITE.name} in your browser</h2>
          </div>
          <button
            type="button"
            className="mux-close"
            onClick={() => setOpen(false)}
            aria-label="Close live terminal"
          >
            Close <span aria-hidden="true">×</span>
          </button>
        </header>
        <div
          className="mux-mode-row"
          role="group"
          aria-label="Terminal runtime"
        >
          <button
            type="button"
            aria-pressed={mode === "demo"}
            onClick={() => setMode("demo")}
          >
            Edge tour <small>no sign-in</small>
          </button>
          <button
            type="button"
            aria-pressed={mode === "native"}
            onClick={() => setMode("native")}
          >
            Linux shell <small>sign-in required</small>
          </button>
        </div>
        {authError && (
          <p className="mux-dialog-description" role="alert">
            {authError}
          </p>
        )}
        <p className="mux-dialog-description" id={`${id}-description`}>
          {mode === "demo" ? (
            <>
              Split with the controls below or <kbd>Ctrl+a</kbd>, then <kbd>%</kbd>.
              Each pane runs a curated, OS-less shell over the {SITE.name} protocol.
              No processes or network. Try <code>help</code>, <code>ls</code>, or{" "}
              <code>demo</code>.
            </>
          ) : (
            <>
              A disposable Linux environment. Split and switch terminals
              directly in this client using the buttons below or{" "}
              <kbd>Ctrl+a</kbd> then <kbd>%</kbd>. Native capacity is limited;
              edge is the fallback.
            </>
          )}
        </p>
        {open &&
          (Terminal ? (
            <Terminal
              key={mode}
              wsUrl={wsUrl}
              mode={mode}
              cols={110}
              rows={24}
              autoStart
              focusOnStart
            />
          ) : (
            <div className="mux-loading" role="status">
              {loadFailed ? (
                <>
                  <p>The terminal client could not download.</p>
                  <button type="button" onClick={loadTerminal}>
                    Retry download
                  </button>
                </>
              ) : (
                "Loading the terminal client…"
              )}
            </div>
          ))}
        <footer className="mux-dialog-foot">
          <span>
            Switching runtime or closing this window releases the hosted
            session.
          </span>
          <a href="/embed">
            Open standalone <span aria-hidden="true">↗</span>
          </a>
        </footer>
      </dialog>
    </div>
  );
}

function TerminalView({
  label,
  separate = false,
}: {
  label: string;
  separate?: boolean;
}) {
  return (
    <div className="mux-pane">
      <header>
        <span>{label}</span>
        <b>terminal {separate ? "02" : "01"}</b>
      </header>
      <div className="mux-code">
        <p>
          <i>$</i> {separate ? "python" : "./build"}
        </p>
        <p className="mux-output">
          {separate ? "a separate process" : "compiling workspace"}
        </p>
        <p className="mux-output">
          {separate ? "a different terminal" : "checking interfaces"}
        </p>
        <p className={separate ? "mux-accent" : "mux-success"}>
          {separate ? ">>>" : "ready for what’s next"}
          <span className="mux-caret" aria-hidden="true" />
        </p>
      </div>
      <footer>
        <span className="mux-dot" />
        {separate ? "independent · same workspace" : "attached · terminal 01"}
      </footer>
    </div>
  );
}
