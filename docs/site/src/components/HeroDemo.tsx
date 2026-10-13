import { useEffect, useId, useRef, useState } from "react";
import type PhuxTerminal from "./PhuxTerminal";
import { SITE, docsHref } from "../lib/site";
import "./HeroDemo.css";

type Mode = "demo" | "native";

export default function HeroDemo({ wsUrl = "" }: { wsUrl?: string }) {
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

  // Any [data-demo-launch] link on the page opens the dialog; without
  // JavaScript it falls through to the standalone shell at its href.
  useEffect(() => {
    function onClick(event: MouseEvent) {
      if (event.defaultPrevented || event.button !== 0) return;
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      const target = event.target instanceof Element ? event.target : null;
      if (!target?.closest("[data-demo-launch]")) return;
      event.preventDefault();
      launch();
    }
    document.addEventListener("click", onClick);
    return () => document.removeEventListener("click", onClick);
  }, []);

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

  return (
    <div className="hero-demo">
      <a
        className="hero-term terminal-surface"
        href="/embed"
        data-demo-launch
        aria-label="Open terminal demo"
      >
        <span className="hero-term-bar">
          <span>{SITE.name} <span className="hero-term-preview">/ preview</span></span>
          <span className="hero-term-try">Open terminal <span aria-hidden="true">↗</span></span>
        </span>
        <span className="hero-term-panes">
          <span className="hero-term-pane">
            <span><i>$</i> cargo build --release</span>
            <span className="dim">Compiling phux-server</span>
            <span className="ok">Finished in 41s</span>
            <span><i>$</i> <b className="caret" /></span>
          </span>
          <span className="hero-term-pane">
            <span><i>$</i> claude</span>
            <span className="dim">Planned the staging migration.</span>
            <span className="ask">Apply it to staging? [y/n]</span>
          </span>
        </span>
        <span className="hero-term-status">
          <span>1 build</span>
          <span>2 agent</span>
          <span className="ask">1 needs input</span>
        </span>
      </a>
      <noscript>
        <p>
          <a href="/embed">Open the hosted shell</a> or{" "}
          <a href={docsHref("/quickstart")}>start locally</a>.
        </p>
      </noscript>
      <dialog
        ref={dialog}
        className="mux-dialog terminal-surface"
        aria-labelledby={`${id}-title`}
        aria-describedby={`${id}-description`}
        onCancel={(event) => {
          event.preventDefault();
          setOpen(false);
        }}
        onClose={() => setOpen(false)}
      >
        <header className="mux-dialog-head">
          <h2 id={`${id}-title`}>{SITE.name}</h2>
          <div className="mux-mode-row" role="group" aria-label="Terminal runtime">
            <button type="button" aria-pressed={mode === "demo"}
              onClick={() => { setAuthError(""); setMode("demo"); }}>Demo</button>
            <button type="button" aria-pressed={mode === "native"}
              onClick={() => setMode("native")}>Linux shell</button>
          </div>
          <button type="button" className="mux-close" onClick={() => setOpen(false)} aria-label="Close live terminal">
            <span aria-hidden="true">×</span>
          </button>
        </header>
        {authError && (
          <p className="mux-dialog-description" role="alert">
            {authError}
          </p>
        )}
        <p className="mux-dialog-description" id={`${id}-description`}>
          {mode === "demo" ? (
            <>A limited demo shell, no sign-in. Try <code>help</code> or <code>demo</code>, then split a pane.</>
          ) : (
            <>A disposable Linux shell. Sign-in required; limited capacity with an edge fallback.</>
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
