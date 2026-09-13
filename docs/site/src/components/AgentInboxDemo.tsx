import { useEffect, useState } from "react";
import {
  attentionDemoDuration,
  nextAttentionDemoPhase,
  type AttentionDemoPhase,
} from "./attention-demo/state";
import "./AgentInboxDemo.css";

const copy: Record<
  AttentionDemoPhase,
  {
    agentState: string;
    inboxState: string;
    terminal: string;
    event: string;
    announcement: string;
  }
> = {
  working: {
    agentState: "working",
    inboxState: "all clear",
    terminal: "checking migration plan against staging schema…",
    event: "tool_start / inspect_schema",
    announcement: "The agent is working. The inbox is clear.",
  },
  blocked: {
    agentState: "blocked",
    inboxState: "1 needs you",
    terminal: "Apply the migration to staging? [approve / revise]",
    event: "ask / approval required",
    announcement: "The agent needs approval. The inbox now points to its terminal.",
  },
  resolved: {
    agentState: "working",
    inboxState: "handled",
    terminal: "approved — migration resumed",
    event: "prompt / operator answered",
    announcement: "The operator answered. The agent resumed work.",
  },
};

export default function AgentInboxDemo() {
  const [phase, setPhase] = useState<AttentionDemoPhase>("working");
  const [replay, setReplay] = useState(0);
  const [reducedMotion, setReducedMotion] = useState(false);

  useEffect(() => {
    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    setReducedMotion(reduced);
    if (reduced) {
      setPhase("blocked");
    }
  }, []);

  useEffect(() => {
    if (reducedMotion) return;
    const timer = window.setTimeout(
      () => setPhase((current) => nextAttentionDemoPhase(current)),
      attentionDemoDuration(phase),
    );
    return () => window.clearTimeout(timer);
  }, [phase, replay, reducedMotion]);

  const current = copy[phase];

  return (
    <div className="attention-demo" data-phase={phase}>
      <header className="attention-demo__bar">
        <span>COCKPIT / WORKSPACE</span>
        <span className="attention-demo__bar-state">{current.inboxState}</span>
      </header>

      <div className="attention-demo__body">
        <aside className="attention-demo__rail" aria-label="Fleet inbox">
          <div className="attention-demo__inbox">
            <span className="attention-demo__inbox-mark" aria-hidden="true">
              {phase === "blocked" ? "!" : phase === "resolved" ? "✓" : "·"}
            </span>
            <span>
              <b>Inbox</b>
              <small>{current.inboxState}</small>
            </span>
          </div>
          <div className="attention-demo__workspace">
            <span className="attention-demo__branch" aria-hidden="true">└</span>
            <span>
              <b>staging deploy</b>
              <small>claude / {current.agentState}</small>
            </span>
          </div>
        </aside>

        <div className="attention-demo__terminal">
          <div className="attention-demo__terminal-head">
            <span>staging deploy</span>
            <span>terminal 07</span>
          </div>
          <div className="attention-demo__screen">
            <p><span className="attention-demo__prompt">$</span> phux agent run deploy-check</p>
            <p className="attention-demo__output">{current.terminal}</p>
            {phase === "blocked" && (
              <p className="attention-demo__choices">
                <span>approve</span>
                <span>revise</span>
              </p>
            )}
          </div>
          <div className="attention-demo__wire">
            <span>AgentSession</span>
            <span>{current.event}</span>
          </div>
        </div>
      </div>

      <footer className="attention-demo__foot">
        <span role="status" aria-live="polite" aria-atomic="true">
          {current.announcement}
        </span>
        <button
          type="button"
          onClick={() => {
            if (reducedMotion) {
              setPhase((current) => nextAttentionDemoPhase(current));
              return;
            }
            setPhase("working");
            setReplay((value) => value + 1);
          }}
        >
          {reducedMotion ? "Next step" : "Replay sequence"}
        </button>
      </footer>
    </div>
  );
}
