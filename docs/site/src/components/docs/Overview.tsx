/** Docs overview body — docs.x.ai/overview IA, phux terminal chrome. */
export function OverviewBody() {
  return (
    <div className="docs-overview">
      <p className="overview-lede">
        Every interface — the TUI, Cockpit, the browser, a script, or an
        agent — talks to the same live terminals. Nobody screen-scrapes.
        Nobody holds a second copy.
      </p>

      <section aria-label="Surfaces">
        <h2>Surfaces</h2>
        <p>
          Same objects, many consumers. Start on the glass you already have.
        </p>
        <div className="overview-apps">
          <a href="/consumers/tui">
            <small>01</small>
            <b>CLI</b>
            <span>The reference TUI. Attach, split, detach. Prefix keys, copy-mode, fleet overlay.</span>
          </a>
          <a href="/consumers/cockpit">
            <small>02</small>
            <b>Cockpit</b>
            <span>Native macOS app for the same terminals. Apple silicon, same wire.</span>
          </a>
          <a href="/consumers/web">
            <small>03</small>
            <b>Web</b>
            <span>Browser client with its own engine. The live demo on phux.sh is this surface.</span>
          </a>
          <a href="/consumers/agents">
            <small>04</small>
            <b>Agents</b>
            <span>CLI, JSON, MCP, OpenCode, Pi, Claude. Read, act, wait on the terminals you see.</span>
          </a>
        </div>
      </section>

      <section aria-label="Pick your path">
        <h2>Pick your path</h2>
        <div className="overview-paths">
          <a href="/quickstart">
            <b>New here</b>
            <span>Install, attach, detach, and drive a pane from a second terminal. No protocol required.</span>
          </a>
          <a href="/concepts">
            <b>Coming from tmux</b>
            <span>The model is familiar until it isn&apos;t: panes are a view. The terminal underneath is an object on a wire.</span>
          </a>
          <a href="/consumers/agents">
            <b>You run agents</b>
            <span>The loop is read, act, wait. Selectors name panes. Agent sessions are a second resource kind.</span>
          </a>
          <a href="/wire">
            <b>Building a peer</b>
            <span>Server sends terminal bytes. Client sends structured input. Start at PROTO, then required L1.</span>
          </a>
        </div>
      </section>

      <div className="overview-columns">
        <section>
          <h2>Get started</h2>
          <ul>
            <li><a href="/quickstart">Quickstart</a></li>
            <li><a href="/quickstart/install">Install</a></li>
            <li><a href="/concepts">Concepts</a></li>
            <li><a href="/concepts/when-to-use">When to use phux</a></li>
          </ul>
        </section>
        <section>
          <h2>Build</h2>
          <ul>
            <li><a href="/consumers/agents">Agent loop</a></li>
            <li><a href="/consumers/mcp">MCP adapter</a></li>
            <li><a href="/consumers/cockpit">Cockpit</a></li>
            <li><a href="/wire/tutorial">Wire tutorial</a></li>
          </ul>
        </section>
        <section>
          <h2>Resources</h2>
          <ul>
            <li><a href="/wire">Protocol</a></li>
            <li><a href="/reference">Generated reference</a></li>
            <li><a href="/architecture">Architecture</a></li>
            <li><a href="/decisions">Decisions</a></li>
          </ul>
        </section>
      </div>
    </div>
  );
}
