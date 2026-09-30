import { useEffect } from "react";

/** Add controls to the existing static code; never replace its selectable text. */
export function CodeControls({ pathname }: { pathname: string }) {
  useEffect(() => {
    const cleanups = Array.from(document.querySelectorAll<HTMLPreElement>("#nd-page .prose pre"), enhanceCode);
    return () => cleanups.forEach((cleanup) => cleanup());
  }, [pathname]);
  return null;
}

function enhanceCode(pre: HTMLPreElement) {
  const shell = document.createElement("div");
  shell.className = "docs-code";
  const toolbar = document.createElement("div");
  toolbar.className = "docs-code-toolbar";
  const status = document.createElement("span");
  status.setAttribute("role", "status");
  status.setAttribute("aria-live", "polite");
  const button = document.createElement("button");
  button.type = "button";
  button.textContent = "Copy code";
  toolbar.append(status, button);
  pre.before(shell);
  shell.append(toolbar, pre);
  const originalTabIndex = pre.getAttribute("tabindex");
  pre.tabIndex = 0;
  let disposed = false;
  let timeout: number | undefined;

  async function copy() {
    clearTimeout(timeout);
    status.textContent = "";
    try {
      await navigator.clipboard.writeText(pre.textContent ?? "");
      if (disposed) return;
      button.textContent = "Copied";
      status.textContent = "Code copied to clipboard.";
    } catch {
      if (disposed) return;
      status.textContent = "Copy unavailable. Select the code and copy it manually.";
      pre.focus();
    }
    timeout = window.setTimeout(() => {
      button.textContent = "Copy code";
      status.textContent = "";
    }, 5000);
  }

  button.addEventListener("click", copy);
  return () => {
    disposed = true;
    clearTimeout(timeout);
    button.removeEventListener("click", copy);
    if (originalTabIndex === null) pre.removeAttribute("tabindex");
    else pre.setAttribute("tabindex", originalTabIndex);
    shell.replaceWith(pre);
  };
}
