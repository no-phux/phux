// The Fumadocs docs chrome as a React island. Everything except the page
// content is Fumadocs: sidebar tree, TOC, prev/next, search, theme. Themed to
// the phux palette via the --color-fd-* overrides in global.css.
import { DocsLayout } from "fumadocs-ui/layouts/docs";
import {
  DocsPage,
  DocsTitle,
  DocsDescription,
  DocsBody,
  EditOnGitHub,
  type DocsPageProps,
} from "fumadocs-ui/layouts/docs/page";
import type { Root } from "fumadocs-core/page-tree";
import type { ReactNode } from "react";
import { navigate } from "astro:transitions/client";
import { RootProvider } from "fumadocs-ui/provider/astro";
import type { AstroProviderProps } from "fumadocs-core/framework/astro";
import { DOCS_NAV } from "../../lib/site";
import SearchDialogComponent from "./search";
import { PierreCodeEnhancer } from "./PierreCodeEnhancer";

interface Props {
  tree: Root;
  children: ReactNode;
  pathname: string;
  params: AstroProviderProps["params"];
  page?: DocsPageProps;
  /** GitHub blob URL for "edit on GitHub" (from the entry's sourcePath). */
  sourceUrl?: string;
  title: string;
  summary: string;
  description: string;
  codeLanguages: string[];
  stability: "stable" | "evolving" | "draft";
  protocolVersion: string;
}

export function Docs({
  tree,
  children,
  pathname,
  params,
  page,
  sourceUrl,
  title,
  summary,
  description,
  codeLanguages,
  stability,
  protocolVersion,
}: Props) {
  const isProtocol = pathname === "/wire" || pathname.startsWith("/wire/");
  const isProtocolHome = pathname === "/wire" || pathname === "/wire/";
  const isOverview = pathname === "/overview" || pathname === "/overview/";
  return (
    <RootProvider
      pathname={pathname}
      params={params}
      navigate={navigate}
      theme={{ enabled: false }}
      search={{ SearchDialog: SearchDialogComponent }}
    >
      <DocsLayout
        tree={tree}
        themeSwitch={{ enabled: false }}
        nav={{
          title: (
            <span>phux <span className="docs-wordmark">docs</span></span>
          ),
          url: "/overview",
        }}
        links={DOCS_NAV.map((item) =>
          "external" in item && item.external
            ? { type: "main", text: item.label, url: item.href, external: true }
            : { type: "main", text: item.label, url: item.href },
        )}
      >
        <DocsPage {...page}>
          <nav className="docs-crumb" aria-label="Breadcrumb">
            <ol>
              <li><a href="/overview">overview</a></li>
              {isProtocol && <li><a href="/wire">wire</a></li>}
              {!isOverview && <li aria-current="page">{title}</li>}
            </ol>
          </nav>
          <DocsTitle>{title}</DocsTitle>
          {!isOverview && <DocsDescription>{description}</DocsDescription>}
          {!isOverview && (
            <div className="docs-meta" aria-label="Document provenance">
              {isProtocol && <span className="meta-primary">wire {protocolVersion}</span>}
              <span>{stability} document</span>
            </div>
          )}
          {summary !== description && !isOverview && (
            <details className="docs-source-summary">
              <summary>Full source summary</summary>
              <p>{summary}</p>
            </details>
          )}
          {isProtocolHome && <ProtocolPrimer version={protocolVersion} />}
          {!isProtocolHome && !isOverview && <SectionPrimer pathname={pathname} />}
          {isOverview ? children : <DocsBody>{children}</DocsBody>}
          {!isOverview && <PierreCodeEnhancer languages={codeLanguages} />}
          {sourceUrl && <EditOnGitHub href={sourceUrl}>View exact source</EditOnGitHub>}
        </DocsPage>
      </DocsLayout>
    </RootProvider>
  );
}

const sectionContent: Record<string, { label: string; intro: string; links: [string, string, string][] }> = {
  "/docs": {
    label: "Start here",
    intro: "You do not need the protocol to use phux. Pick the shortest path for what you are trying to do.",
    links: [
      ["Decide", "/concepts/when-to-use", "Whether phux fits you today."],
      ["Run it", "/quickstart", "Install, attach, detach, drive it from a second terminal."],
      ["The model", "/concepts", "What a terminal is on the wire."],
    ],
  },
  "/consumers": {
    label: "Choose an interface",
    intro: "Every interface is a peer over the same terminals. Choose by operator, not by protocol privilege.",
    links: [
      ["TUI", "/consumers/tui", "Interactive sessions, splits, and local navigation."],
      ["Cockpit", "/consumers/cockpit", "Native macOS client for the same terminals."],
      ["Agents", "/consumers/agents", "CLI, JSON, MCP, and host integrations."],
    ],
  },
  "/consumers/agents": {
    label: "Agent path",
    intro: "Start with the loop. Open JSON and session verbs only when you need them.",
    links: [
      ["The loop", "/consumers/agents#2-the-loop", "Read, act, wait, read again."],
      ["Selectors", "/consumers/agents#3-selectors", "How to name a pane, including %name."],
      ["Agent sessions", "/consumers/agents#6-agentsession-verbs-vs-detector-verbs", "The second resource kind versus the pane detector."],
    ],
  },
  "/consumers/tui": {
    label: "Interactive path",
    intro: "Attach, split, detach. Commands and chrome live on this page; the TOC is the map.",
    links: [
      ["First minutes", "/consumers/tui#first-minutes", "Prefix keys and detach."],
      ["Selectors", "/consumers/tui#selectors", "How to name a pane."],
      ["Keys", "/consumers/tui#keys", "Prefix table, cheat sheet, copy-mode."],
    ],
  },
  "/reference": {
    label: "Generated from the binary",
    intro: "Exact commands, defaults, actions, widgets, hooks, exit codes, and paths. These pages fail CI if they drift from phux.",
    links: [
      ["CLI", "/reference/cli", "Every invocation path, flag, default, and help string."],
      ["Configuration", "/reference/config", "Accepted sections, values, defaults, and examples."],
      ["Runtime contracts", "/reference/exit-codes", "Exit codes, files, hooks, actions, and deprecations."],
    ],
  },
  "/architecture": {
    label: "Understand the implementation",
    intro: "Architecture explains what the code is. The protocol defines interoperability; decisions explain why the shape exists.",
    links: [
      ["Glance diagram", "/architecture/diagram", "PTY in, resource engines, the frame seam, client replicas."],
      ["Process model", "/architecture/process-model", "One server per user, supervision, runtime boundaries."],
      ["State sync", "/architecture/state-sync", "What happens on attach."],
    ],
  },
  "/decisions": {
    label: "Project record",
    intro: "Read a decision when you need the reason a design space was closed. Use architecture for the current system shape.",
    links: [
      ["Wire model", "/decisions/adr-0013", "Why terminal bytes remain terminal bytes."],
      ["Peer consumers", "/decisions/adr-0017", "Why the TUI has no protocol privilege."],
      ["Projection model", "/decisions/adr-0030", "Why structured views stay consumer-owned."],
    ],
  },
};

function SectionPrimer({ pathname }: { pathname: string }) {
  const section = sectionContent[pathname.replace(/\/$/, "")];
  if (!section) return null;
  return (
    <section className="section-primer" aria-label={section.label}>
      <small>{section.label}</small>
      <p>{section.intro}</p>
      <div>
        {section.links.map(([title, href, text]) => (
          <a href={href} key={href}><b>{title}</b><span>{text}</span></a>
        ))}
      </div>
    </section>
  );
}

function ProtocolPrimer({ version }: { version: string }) {
  return (
    <section className="protocol-primer" aria-label="Protocol at a glance">
      <div className="protocol-flow" aria-label="Connection lifecycle">
        <span>HELLO</span><i>01</i><span>ATTACH</span><i>02</i><span>BOOTSTRAP</span><i>03</i><span>OUTPUT / INPUT</span><i>04</i><span>DETACH</span>
      </div>
      <p className="protocol-thesis"><b>Server sends terminal bytes.</b> Client sends structured input. Both sides run the same terminal engine, so there is no second screen model in the middle.</p>
      <div className="protocol-tiers">
        <a href="/wire/proto"><small>required</small><b>PROTO</b><span>framing, negotiation, lifecycle</span></a>
        <a href="/wire/l1"><small>required</small><b>L1</b><span>terminal bytes, input, synchronization</span></a>
        <a href="/wire/l3"><small>optional</small><b>L3</b><span>metadata, links, shared structure</span></a>
      </div>
      <div className="protocol-paths">
        <a href="/wire/tutorial"><b>Learn the flow</b><span>One complete {version} session, end to end.</span></a>
        <a href="/wire/proto"><b>Build a peer</b><span>Start with framing, then implement required L1.</span></a>
        <a href="/wire/appendix-encoding"><b>Check the bytes</b><span>Normative primitives, payload shapes, and ranges.</span></a>
      </div>
    </section>
  );
}
