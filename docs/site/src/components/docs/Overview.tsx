import { OVERVIEW, SITE } from "../../lib/site";
import { AgentStart } from "./AgentStart";

export function OverviewBody() {
  const { pitch } = OVERVIEW;
  return (
    <div className="docs-overview">
      <section className="overview-pitch" aria-labelledby="overview-pitch-title">
        <h2 id="overview-pitch-title">{pitch.headline}</h2>
        <p>{pitch.text}</p>
      </section>

      <div className="overview-actions">
        <a className="overview-primary" href="/quickstart/install">Install phux <span aria-hidden="true">→</span></a>
        <a href={`${SITE.url}/embed`}>Try in your browser <span aria-hidden="true">↗</span></a>
        <a href="#overview-agent-title">Use with an agent <span aria-hidden="true">↓</span></a>
      </div>

      <section className="overview-first-run" aria-labelledby="overview-first-run-title">
        <div className="overview-first-run-heading">
          <h2 id="overview-first-run-title">Try it</h2>
          <a href="/quickstart">Full quickstart →</a>
        </div>
        <ol className="overview-first-run-steps">
          {OVERVIEW.firstRun.map(({ label, action, detail }) => (
            <li key={label}>
              <span className="overview-step-label">{label}</span>
              <code>{action}</code>
              <span>{detail}</span>
            </li>
          ))}
        </ol>
        <p className="overview-first-run-result">Detach leaves it running. A server crash or reboot ends it. <a href="/quickstart#what-survives">Continuity details →</a></p>
      </section>

      <section aria-labelledby="overview-model-title">
        <h2 id="overview-model-title">The model</h2>
        <figure className="overview-model" aria-labelledby="overview-model-caption">
          <div className="overview-model-server">
            <strong>Server</strong>
            <div className="overview-model-sessions">
              <div className="overview-model-session">
                <strong>Session</strong>
                <div className="overview-model-resource">
                  <strong>Resource · terminal</strong><span>shell + output</span>
                </div>
              </div>
              <div className="overview-model-inbox"><strong>Inbox · session</strong><span>Unorganized resources</span></div>
            </div>
          </div>
          <div className="overview-model-clients">
            <span><strong>View · TUI pane</strong></span>
            <span><strong>View · desktop tile</strong></span>
            <span><strong>Agent · read / type</strong></span>
          </div>
          <figcaption id="overview-model-caption">Sessions own resources. Arrangements place them. Views show them. <a href="/concepts">Full model →</a></figcaption>
        </figure>
      </section>

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

      <AgentStart />

      <section className="overview-reference" aria-labelledby="overview-reference-title">
        <div>
          <h2 id="overview-reference-title">Reference</h2>
        </div>
        <div className="overview-reference-links">
          <a href="/concepts">Core concepts</a>
          <a href="/reference">CLI and configuration</a>
          <a href="/wire">Wire protocol</a>
          <a href="/architecture">Architecture</a>
          <a href="/concepts/when-to-use">Compare tools</a>
          <a href="/troubleshooting">Troubleshooting</a>
          <a href="/docs">All documentation</a>
        </div>
      </section>
    </div>
  );
}
