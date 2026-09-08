import type { FileContents } from "@pierre/diffs";
import { createRoot, type Root } from "react-dom/client";
import { useEffect } from "react";

const extensions: Record<string, string> = {
  bash: "sh",
  console: "sh",
  html: "html",
  json: "json",
  markdown: "md",
  rust: "rs",
  sh: "sh",
  text: "txt",
  toml: "toml",
  yaml: "yaml",
};

export function PierreCodeEnhancer({ languages }: { languages: string[] }) {
  useEffect(() => {
    const roots: Root[] = [];
    const fallbacks: HTMLElement[] = [];
    let disposed = false;

    const blocks = document.querySelectorAll<HTMLElement>(
      "#nd-page .prose pre:not([data-pierre-enhanced])",
    );
    if (blocks.length > 0) void enhance();

    async function enhance() {
      const { File } = await import("@pierre/diffs/react");
      if (disposed) return;

      for (const [index, pre] of [...blocks].entries()) {
        const language = languages[index] || "text";
        const contents = pre.textContent?.replace(/\n$/, "") ?? "";
        if (!contents) continue;

        pre.dataset.pierreEnhanced = "true";
        const shell = document.createElement("div");
        shell.className = "pierre-code-shell";
        pre.before(shell);
        shell.append(pre);

        const mount = document.createElement("div");
        mount.className = "pierre-code-mount";
        shell.append(mount);

        const root = createRoot(mount);
        roots.push(root);
        fallbacks.push(pre);
        root.render(
          <File
            file={{
              name: `snippet.${extensions[language] ?? language}`,
              contents,
              lang: language as FileContents["lang"],
            }}
            options={{
              disableFileHeader: true,
              overflow: "scroll",
              theme: "pierre-dark",
              themeType: "dark",
              lineHoverHighlight: "line",
              enableLineSelection: true,
            }}
            disableWorkerPool
          />,
        );

        // Keep the static block until Pierre has painted its shadow tree.
        let attempts = 0;
        const reveal = () => {
          if (disposed) return;
          const rendered = mount.querySelector("diffs-container")?.shadowRoot?.querySelector("pre");
          if (rendered) {
            pre.hidden = true;
            mount.dataset.ready = "true";
            return;
          }
          attempts++;
          if (attempts >= 180) {
            root.unmount();
            roots.splice(roots.indexOf(root), 1);
            fallbacks.splice(fallbacks.indexOf(pre), 1);
            mount.remove();
            pre.removeAttribute("data-pierre-enhanced");
            return;
          }
          requestAnimationFrame(reveal);
        };
        requestAnimationFrame(reveal);
      }
    }

    return () => {
      disposed = true;
      for (const root of roots) root.unmount();
      for (const fallback of fallbacks) {
        fallback.hidden = false;
        fallback.removeAttribute("data-pierre-enhanced");
      }
    };
  }, [languages]);

  return null;
}
