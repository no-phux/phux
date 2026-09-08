import { defineConfig } from "astro/config";
import react from "@astrojs/react";
import sitemap from "@astrojs/sitemap";
import tailwindcss from "@tailwindcss/vite";
import mdx from "@astrojs/mdx";
import { fileURLToPath } from "node:url";
import { unified } from "@astrojs/markdown-remark";
import {
  rehypeCode,
  remarkCodeTab,
  remarkHeading,
  remarkNpm,
} from "fumadocs-core/mdx-plugins";
import { buildSearchIndex } from "./scripts/build-search.mjs";

// The docs pages render with the Fumadocs markdown pipeline: anchored headings
// (for the TOC), npm-install tabs, and Shiki-highlighted code.
const remarkPlugins = [remarkHeading, remarkCodeTab, remarkNpm];
const rehypePlugins = [rehypeCode];

// Tables keep their semantics, while the wrapper provides keyboard scrolling
// and an announced region on narrow screens.
function rehypeAccessibleTables() {
  return (tree) => {
    function visit(node) {
      if (!node.children) return;
      node.children = node.children.map((child) => {
        if (child.type === "element" && child.tagName === "table") {
          return {
            type: "element",
            tagName: "div",
            properties: {
              className: ["table-region"],
              role: "region",
              tabIndex: 0,
              ariaLabel: "Scrollable data table",
            },
            children: [child],
          };
        }
        visit(child);
        return child;
      });
    }
    visit(tree);
  };
}

rehypePlugins.push(rehypeAccessibleTables);

// Emits the static search index into dist/api/search.json after every build so
// the site deploys fully static (see scripts/build-search.mjs).
const searchIndex = () => ({
  name: "phux-static-search",
  hooks: {
    "astro:build:done": async ({ dir }) => {
      await buildSearchIndex(fileURLToPath(dir));
    },
  },
});

export default defineConfig({
  site: "https://phux.sh",
  // Match the wire/phall.io house style: bare, no-trailing-slash canonical paths.
  trailingSlash: "never",
  build: { format: "file" },
  // The live terminal is a React island (<PhuxTerminal client:idle />), and the
  // docs chrome is the Fumadocs React island (<Docs client:load />).
  integrations: [react(), mdx({ extendMarkdownConfig: true, syntaxHighlight: false }), sitemap(), searchIndex()],
  markdown: {
    processor: unified({
      syntaxHighlight: false,
      remarkPlugins,
      rehypePlugins,
    }),
  },
  vite: {
    plugins: [tailwindcss()],
    // The phux-web client ships a .wasm asset (the wire client + embedded
    // libghostty-vt engine); keep it un-inlined so it streams.
    assetsInclude: ["**/*.wasm"],
  },
});
