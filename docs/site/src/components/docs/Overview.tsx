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
      </section>

      <div className="overview-actions">
        <a className="overview-primary" href="/quickstart">Quickstart <span aria-hidden="true">→</span></a>
        <a href="/concepts">How phux works <span aria-hidden="true">→</span></a>
      </div>

      <figure className="overview-model" aria-labelledby="overview-model-caption">
        <div className="overview-model-server">
          <strong>Server</strong><span>One background process owns the work.</span>
        </div>
        <div className="overview-model-sessions">
          <div><strong>Session: my-project</strong><span>Terminal · Terminal · Agent conversation</span></div>
          <div><strong>Inbox</strong><span>A session for work you have not organized yet.</span></div>
        </div>
        <div className="overview-model-clients">TUI · desktop · browser · agent <span>read or control the same resources</span></div>
        <figcaption id="overview-model-caption">A session groups resources. Visual clients show them as views; closing a view leaves the resource running.</figcaption>
      </figure>

      <section className="overview-terms" aria-labelledby="overview-terms-title">
        <h2 id="overview-terms-title">Five words to know</h2>
        <div className="overview-term-grid">
          {OVERVIEW.terms.map(({ name, text }) => (
            <div key={name}><h3>{name}</h3><p>{text}</p></div>
          ))}
        </div>
        <p className="overview-lifetime"><strong>Close</strong> a view or detach to leave work running. <strong>Kill</strong> a resource to end it. A session can keep its resources even when no client is attached. <a href="/concepts">Read the full model →</a></p>
      </section>

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
