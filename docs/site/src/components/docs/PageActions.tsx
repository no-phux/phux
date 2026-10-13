import { useState } from "react";

/** A stable link works without JavaScript; copying is progressive enhancement. */
export function PageActions({ pathname }: { pathname: string }) {
  const [status, setStatus] = useState("");
  const [copying, setCopying] = useState(false);
  const markdownUrl = `${pathname.replace(/\/$/, "")}.md`;

  async function readMarkdown() {
    const response = await fetch(markdownUrl, { signal: AbortSignal.timeout(10000) });
    if (!response.ok) throw new Error("Markdown unavailable");
    return new Blob([await response.text()], { type: "text/plain" });
  }

  async function copyPage() {
    setCopying(true);
    setStatus("");
    try {
      // Start the clipboard write in the click gesture, not after the fetch.
      // WebKit requires this even when the content itself arrives asynchronously.
      await navigator.clipboard.write([new ClipboardItem({ "text/plain": readMarkdown() })]);
      setStatus("Page copied as Markdown.");
    } catch {
      setStatus("Copy unavailable. Open Markdown and copy the text manually.");
    } finally {
      setCopying(false);
    }
  }
  return (
    <div className="docs-page-actions" data-page-actions>
      <button type="button" onClick={copyPage} disabled={copying}>Copy page for agent</button>
      <a href={markdownUrl}>Open Markdown</a>
      <span role="status" aria-live="polite">{status}</span>
    </div>
  );
}
