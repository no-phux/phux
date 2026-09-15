import { describe, expect, test } from "bun:test";
import {
  estimateTokens,
  extractContent,
  frontmatter,
  htmlToMarkdown,
  wantsMarkdown,
} from "./markdown";

const DOC = `<!doctype html>
<html>
<head>
  <title>Quickstart — phux</title>
  <meta name="description" content="Install phux and attach to your first terminal." />
  <meta property="og:image" content="https://phux.sh/og.png" />
</head>
<body>
  <header class="site-frame"><nav><a href="/">phux</a></nav></header>
  <main id="main">
    <h1>Quickstart</h1>
    <p>Install phux, then <a href="/install">run the installer</a>.</p>
    <ul><li>one</li><li>two</li></ul>
    <pre><code>curl -fsSL https://phux.sh/install.sh | sh</code></pre>
    <script>window.hydrate()</script>
  </main>
  <footer><nav>footer links</nav></footer>
</body>
</html>`;

describe("wantsMarkdown", () => {
  test("accepts an exact text/markdown request", () => {
    expect(wantsMarkdown("text/markdown")).toBe(true);
  });

  test("accepts markdown mixed with html preferences", () => {
    expect(wantsMarkdown("text/html, text/markdown;q=0.8, */*;q=0.1")).toBe(true);
    expect(wantsMarkdown("text/markdown, text/html")).toBe(true);
  });

  test("rejects q=0 and non-markdown accepts", () => {
    expect(wantsMarkdown("text/markdown;q=0")).toBe(false);
    expect(wantsMarkdown("text/html,application/xhtml+xml")).toBe(false);
    expect(wantsMarkdown(null)).toBe(false);
    expect(wantsMarkdown("")).toBe(false);
    expect(wantsMarkdown("text/markdownx")).toBe(false);
  });
});

describe("extractContent", () => {
  test("keeps main and drops page chrome", () => {
    const content = extractContent(DOC);
    expect(content).toContain("<h1>Quickstart</h1>");
    expect(content).toContain("run the installer");
    expect(content).not.toContain("site-frame");
    expect(content).not.toContain("footer links");
    expect(content).not.toContain("window.hydrate");
  });

  test("falls back to body when main is absent", () => {
    const html = `<body><nav>chrome</nav><article><p>prose</p></article></body>`;
    const content = extractContent(html);
    expect(content).toContain("prose");
    expect(content).not.toContain("chrome");
  });
});

describe("frontmatter", () => {
  test("emits title, description, and image from meta tags", () => {
    expect(frontmatter(DOC)).toBe(
      `---\ntitle: "Quickstart — phux"\ndescription: "Install phux and attach to your first terminal."\nimage: "https://phux.sh/og.png"\n---`,
    );
  });

  test("falls back to og:title and the title element", () => {
    const html = `<head><meta property="og:title" content="OG Title" /></head><body></body>`;
    expect(frontmatter(html)).toContain('title: "OG Title"');
    const bare = `<head><title>Bare Title</title></head><body></body>`;
    expect(frontmatter(bare)).toContain('title: "Bare Title"');
  });

  test("returns empty string when no meta tags exist", () => {
    expect(frontmatter("<html><body><p>x</p></body></html>")).toBe("");
  });
});

describe("htmlToMarkdown", () => {
  test("converts a page to markdown with frontmatter and no chrome", () => {
    const markdown = htmlToMarkdown(DOC);
    expect(markdown.startsWith("---\ntitle:")).toBe(true);
    expect(markdown).toContain("# Quickstart");
    expect(markdown).toContain("[run the installer](/install)");
    expect(markdown).toContain("- one");
    expect(markdown).toContain("```");
    expect(markdown).toContain("curl -fsSL https://phux.sh/install.sh | sh");
    expect(markdown).not.toContain("site-frame");
    expect(markdown).not.toContain("footer links");
    expect(markdown.endsWith("\n")).toBe(true);
  });
});

describe("estimateTokens", () => {
  test("is a positive rough estimate proportional to size", () => {
    expect(estimateTokens("")).toBe(1);
    expect(estimateTokens("abcd")).toBe(1);
    expect(estimateTokens("a".repeat(400))).toBe(100);
  });
});
