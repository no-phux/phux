import { OVERVIEW, SITE } from "../../lib/site";
import { AgentStart } from "./AgentStart";

export function OverviewBody() {
  const { pitch } = OVERVIEW;
  return (
    <div className="docs-overview">
      <section className="overview-pitch" aria-labelledby="overview-pitch-title">
        <p className="overview-eyebrow">Your first minute with phux</p>
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
          <h2 id="overview-first-run-title">Try it in a terminal</h2>
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
        <p className="overview-first-run-result"><strong>The payoff:</strong> closing a client leaves its terminal running. A server crash or reboot ends live processes; <a href="/quickstart#what-survives">see what survives</a>.</p>
      </section>

      <section aria-labelledby="overview-model-title">
        <h2 id="overview-model-title">What you just used</h2>
        <figure className="overview-model" aria-labelledby="overview-model-caption">
          <div className="overview-model-server">
            <strong>Server</strong><span>Owns the running work</span>
            <div className="overview-model-sessions">
              <div className="overview-model-session">
                <strong>Session · current</strong>
                <div className="overview-model-resource">
                  <strong>Resource · terminal</strong><span>shell and screen output</span>
                </div>
              </div>
              <div className="overview-model-inbox"><strong>Inbox</strong><span>Another session for unorganized work</span></div>
            </div>
          </div>
          <div className="overview-model-clients">
            <span><strong>TUI pane</strong> shows the terminal</span>
            <span><strong>Desktop tile</strong> shows the same terminal</span>
            <span><strong>Agent</strong> can read or send input</span>
          </div>
          <figcaption id="overview-model-caption">The session owns the resource. An arrangement places it in tabs and splits; a view shows it. Leaving a view does not end the resource.</figcaption>
        </figure>
      </section>

      <section className="overview-terms" aria-labelledby="overview-terms-title">
        <h2 id="overview-terms-title">The five pieces</h2>
        <div className="overview-term-grid">
          {OVERVIEW.terms.map(({ name, text }) => (
            <div key={name}><h3>{name}</h3><p>{text}</p></div>
          ))}
        </div>
        <p className="overview-lifetime"><strong>Close</strong> a view or detach to leave work running. <strong>Kill</strong> a resource to end it. <a href="/concepts">Read the complete model →</a></p>
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
