import { useState } from "react";
import { AGENT_START } from "../../lib/site";

export function AgentStart() {
  const [status, setStatus] = useState("");
  async function copyPrompt() {
    try {
      await navigator.clipboard.writeText(AGENT_START.prompt);
      setStatus("Agent prompt copied.");
    } catch {
      setStatus("Copy unavailable. Select the prompt and copy it manually.");
    }
  }
  return (
    <section className="overview-agent" aria-labelledby="overview-agent-title">
      <h2 id="overview-agent-title">Hand this to your agent</h2>
      <p>Let your agent find the right guide and help verify your first terminal.</p>
      <div className="agent-prompt">
        <pre>{AGENT_START.prompt}</pre>
        <button type="button" onClick={copyPrompt}>Copy agent prompt</button>
        <span role="status" aria-live="polite">{status}</span>
      </div>
      <div className="agent-routes">
        <div>
          <h3>Control your terminals</h3>
          <p>Install the operating skills, then connect your host’s tools to your running phux server.</p>
          <pre><code>{AGENT_START.skills}</code></pre>
          <a href="/consumers/getting-started">Choose your agent integration →</a>
        </div>
        <div>
          <h3>Search the documentation</h3>
          <p>The hosted docs MCP is read-only. It searches guides and releases; it cannot see or control your terminals.</p>
          <pre><code>{AGENT_START.mcp}</code></pre>
          <a href="/consumers/getting-started#let-your-agent-read-the-docs">Connect another MCP client →</a>
        </div>
      </div>
      <p className="agent-discovery">
        <a href="/llms.txt">Agent guide</a> · <a href="https://phux.sh/.well-known/agent-skills/index.json">Skill index</a> · <a href="https://phux.sh/.well-known/mcp/server-card.json">Docs MCP details</a>
      </p>
    </section>
  );
}
