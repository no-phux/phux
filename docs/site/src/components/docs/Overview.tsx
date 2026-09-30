const paths = [
  { href: "/quickstart", title: "Use phux locally", text: "Install, attach, split a pane, and detach without stopping your work.", label: "Start here" },
  { href: "/consumers/getting-started", title: "Run a coding agent", text: "Choose a harness, connect its tools, and share a terminal you can inspect.", label: "For agent users" },
  { href: "/remote-access", title: "Connect another machine", text: "Choose a connection path and reach the terminals running on your host.", label: "Work across hosts" },
  { href: "/concepts/coming-from", title: "Bring your tmux habits", text: "Map sessions, panes, prefix keys, and detach to the phux model.", label: "Make the switch" },
  { href: "/concepts/when-to-use", title: "Decide if phux fits", text: "Understand the tradeoffs alongside tmux, Herdr, and cmux.", label: "Compare options" },
  { href: "/troubleshooting", title: "Get unstuck", text: "Recover from connection, installation, and agent-integration problems.", label: "Find a fix" },
];

export function OverviewBody() {
  return (
    <div className="docs-overview">
      <div className="overview-actions">
        <a className="overview-primary" href="/quickstart">Start the quickstart <span aria-hidden="true">→</span></a>
        <a href="/performance">Explore the performance evidence <span aria-hidden="true">→</span></a>
      </div>

      <figure className="overview-model" aria-labelledby="terminal-model-caption">
        <div className="overview-model-clients">
          <div><strong>You</strong><span>Attach and interact</span></div>
          <div><strong>Your app</strong><span>Display and control</span></div>
          <div><strong>Your agent</strong><span>Read, act, and wait</span></div>
        </div>
        <div className="overview-model-connectors" aria-hidden="true"><span>↕</span><span>↕</span><span>↕</span></div>
        <div className="overview-model-terminal">
          <span className="overview-model-label">One running terminal</span>
          <code><span aria-hidden="true">$ </span>your shell, editor, or long-running task</code>
          <span>phux keeps the process running when clients disconnect.</span>
        </div>
        <figcaption id="terminal-model-caption">Different interfaces. The same work. A person, an app, and an agent can connect to the same terminal; each sees its output and can send input.</figcaption>
      </figure>

      <section aria-labelledby="overview-paths-title">
        <h2 id="overview-paths-title">What do you want to do?</h2>
        <div className="overview-paths">
          {paths.map(({ href, title, text, label }) => (
            <a href={href} key={href}>
              <span className="overview-path-label">{label}</span>
              <h3>{title} <span aria-hidden="true">→</span></h3>
              <p>{text}</p>
            </a>
          ))}
        </div>
      </section>

      <section className="overview-reference" aria-labelledby="overview-reference-title">
        <div>
          <h2 id="overview-reference-title">Go deeper when you need to</h2>
          <p>You do not need the protocol to get started. These are the maps for understanding, automating, and building on phux.</p>
        </div>
        <nav aria-label="Explore the documentation">
          <a href="/concepts">Core concepts</a>
          <a href="/reference">Command and configuration reference</a>
          <a href="/wire">Wire protocol</a>
          <a href="/architecture">Architecture</a>
          <a href="/docs">All documentation</a>
        </nav>
      </section>
    </div>
  );
}
