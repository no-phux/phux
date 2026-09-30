import { useEffect, useId, useRef, useState } from "react";
import type PhuxTerminal from "./PhuxTerminal";
import "./MultiplexShowcase.css";

type View = "split" | "mirror" | "detached";
type Mode = "demo" | "native";
const explanations: Record<View, { title: string; detail: string }> = {
  split: {
    title: "Two terminals. One workspace.",
    detail:
      "A split creates another view into your workspace. Each terminal has its own identity, independent of the pane around it.",
  },
  mirror: {
    title: "Two views. The same terminal.",
    detail:
      "The browser and your desktop can attach to terminal 01 together. Same process, same output—not two shells pretending to stay in sync.",
  },
  detached: {
    title: "Close the view. Keep the work.",
    detail:
      "Detaching a client does not kill its terminal. Reattach to the same identity and pick up where you left off.",
  },
};

export default function MultiplexShowcase({ wsUrl = "" }: { wsUrl?: string }) {
  const [view, setView] = useState<View>("split");
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<Mode>("demo");
  const [Terminal, setTerminal] = useState<typeof PhuxTerminal | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  const [authFailed, setAuthFailed] = useState(false);
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
    setAuthFailed(result === "error");
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
            phux<span aria-hidden="true"> / </span>
            <span>one workspace, many views</span>
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
                <b>Browser detached.</b>
                <span>Terminal 01 is still there.</span>
                <button type="button" onClick={() => setView("mirror")}>
                  Reattach same terminal <span aria-hidden="true">↵</span>
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
          <span>phux protocol</span>
          <span className="mux-wire-line" />
        </div>
        <div className="mux-server">
          <span className="mux-server-icon" aria-hidden="true">
            ▤
          </span>
          <div>
            <b>One phux server</b>
            <span>Terminals live here. Panes are just views.</span>
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
              Open a live terminal
              <small>No install. No sign-in for the edge tour.</small>
            </span>
            <span aria-hidden="true">↗</span>
          </button>
        </div>
      </div>
      <p className="mux-caption">
        Understand it here. Try the real wire in the live terminal.
      </p>
      <noscript>
        <p>
          JavaScript is required for the interactive demo.{" "}
          <a href="/embed">Open the hosted shell</a> or{" "}
          <a href="https://docs.phux.sh/quickstart">start locally</a>.
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
            <span className="mux-eyebrow">NOT A RECORDING</span>
            <h2 id={`${id}-title`}>Your browser is a phux client.</h2>
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
        {authFailed && (
          <p className="mux-dialog-description" role="status">
            Sign-in did not finish. Try a provider again below, or choose the
            edge tour.
          </p>
        )}
        <p className="mux-dialog-description" id={`${id}-description`}>
          {mode === "demo" ? (
            <>
              Real phux protocol and libghostty rendering. A curated shell, not
              a Linux process. Try <code>help</code>, <code>ls</code>, or{" "}
              <code>demo</code>.
            </>
          ) : (
            <>
              A disposable Linux environment. Run <code>phux</code>, then{" "}
              <kbd>Ctrl+a</kbd> followed by <kbd>%</kbd> to split. Native
              capacity is limited; the edge tour is the fallback.
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
