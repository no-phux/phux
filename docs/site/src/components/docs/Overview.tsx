import { OVERVIEW } from "../../lib/site";

export function OverviewBody() {
  return (
    <div className="docs-overview">
      <div className="overview-actions">
        <a className="overview-primary" href="/quickstart">Quickstart <span aria-hidden="true">→</span></a>
        <a href="/performance">Performance <span aria-hidden="true">→</span></a>
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
        <nav aria-label="Explore the documentation">
          <a href="/concepts">Core concepts</a>
          <a href="/reference">CLI and configuration</a>
          <a href="/wire">Wire protocol</a>
          <a href="/architecture">Architecture</a>
          <a href="/docs">All documentation</a>
        </nav>
      </section>
    </div>
  );
}
