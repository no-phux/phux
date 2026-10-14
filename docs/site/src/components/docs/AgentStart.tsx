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
      <h2 id="overview-agent-title">Use an agent</h2>
      <div className="agent-prompt">
        <pre>{AGENT_START.prompt}</pre>
        <button type="button" onClick={copyPrompt}>Copy agent prompt</button>
        <span role="status" aria-live="polite">{status}</span>
      </div>
      <p className="agent-links">
        <a href="/consumers/getting-started">Agent integrations →</a>
        <a href="/consumers/getting-started#let-your-agent-read-the-docs">Docs MCP (read-only) →</a>
      </p>
    </section>
  );
}
