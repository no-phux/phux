import { OVERVIEW } from "../../lib/site";
import { AgentStart } from "./AgentStart";

export function OverviewBody() {
  const { pitch } = OVERVIEW;
  return (
    <div className="docs-overview">
      <section className="overview-pitch" aria-labelledby="overview-pitch-title">
        <h2 id="overview-pitch-title">{pitch.headline}</h2>
        <p>
          {pitch.text}
          <a href={pitch.linkHref}>{pitch.linkLabel}</a>.
        </p>
        {/*
          TODO: iOS availability wording. Patrick has not chosen between a
          TestFlight link and "coming soon". Keep the line below neutral.
          Do not add a TestFlight URL or a coming-soon claim until that
          choice is made.
        */}
        <p className="overview-ios">
          {pitch.iosBefore}
          <a href={pitch.iosHref}>{pitch.iosLabel}</a>
          {pitch.iosAfter}
        </p>
      </section>

      <div className="overview-actions">
        <a className="overview-primary" href="/quickstart">Quickstart <span aria-hidden="true">→</span></a>
        <a href="#overview-agent-title">Set up your agent <span aria-hidden="true">→</span></a>
      </div>

      <figure className="overview-model" aria-labelledby="terminal-model-caption">
        <div className="overview-model-clients">
          <div><strong>Terminal UI</strong><span>Attach and interact</span></div>
          <div><strong>Apps</strong><span>Display and control</span></div>
          <div><strong>Agents</strong><span>Read, act, and wait</span></div>
        </div>
        <div className="overview-model-connectors" aria-hidden="true"><span>↕</span><span>↕</span><span>↕</span></div>
        <div className="overview-model-terminal">
          <span className="overview-model-label">One running terminal</span>
          <code><span aria-hidden="true">$ </span>shell, editor, or long-running task</code>
          <span>Processes keep running after a client disconnects.</span>
        </div>
        <figcaption id="terminal-model-caption">The server owns the terminals. Clients read output and send input through the same protocol.</figcaption>
      </figure>

      <AgentStart />

      <section aria-labelledby="overview-paths-title">
        <h2 id="overview-paths-title">Guides</h2>
        <div className="overview-paths">
          {OVERVIEW.paths.map(({ href, title, text }) => (
            <a href={href} key={href}>
              <h3>{title} <span aria-hidden="true">→</span></h3>
              <p>{text}</p>
            </a>
          ))}
        </div>
      </section>

      <section className="overview-reference" aria-labelledby="overview-reference-title">
        <div>
          <h2 id="overview-reference-title">Reference</h2>
        </div>
        <div className="overview-reference-links">
          <a href="/concepts">Core concepts</a>
          <a href="/reference">CLI and configuration</a>
          <a href="/wire">Wire protocol</a>
          <a href="/architecture">Architecture</a>
          <a href="/docs">All documentation</a>
        </div>
      </section>
    </div>
  );
}
