import { DocsLayout } from "fumadocs-ui/layouts/docs";
import {
  DocsPage,
  DocsTitle,
  DocsDescription,
  DocsBody,
  type DocsPageProps,
} from "fumadocs-ui/layouts/docs/page";
import type { Root } from "fumadocs-core/page-tree";
import type { ReactNode } from "react";
import { navigate } from "astro:transitions/client";
import { RootProvider } from "fumadocs-ui/provider/astro";
import type { AstroProviderProps } from "fumadocs-core/framework/astro";
import { SidebarProvider, SidebarTrigger, useSidebar } from "fumadocs-ui/layouts/docs/slots/sidebar";
import { DOCS_NAV, SITE } from "../../lib/site";
import SearchDialogComponent from "./search";
import { CodeControls } from "./CodeControls";
import { ReaderSidebar, ReaderTheme } from "./ReaderControls";

interface Props {
  tree: Root;
  children: ReactNode;
  pathname: string;
  params: AstroProviderProps["params"];
  page?: DocsPageProps;
  sourceUrl?: string;
  title: string;
  summary: string;
  description: string;
  stability: "stable" | "evolving" | "draft";
  protocolVersion: string;
}

const readerSlots = {
  themeSwitch: ReaderTheme,
  sidebar: {
    provider: SidebarProvider,
    root: ReaderSidebar,
    trigger: SidebarTrigger,
    useSidebar,
  },
};

export function Docs({
  tree, children, pathname, params, page, sourceUrl,
  title, summary, description, stability, protocolVersion,
}: Props) {
  const isOverview = pathname.replace(/\/$/, "") === "/overview";
  const isProtocol = pathname === "/wire" || pathname.startsWith("/wire/");
  return (
    <RootProvider
      pathname={pathname}
      params={params}
      navigate={navigate}
      theme={{ defaultTheme: "system", enableSystem: true, disableTransitionOnChange: true, hotKey: false }}
      search={{ SearchDialog: SearchDialogComponent }}
    >
      <DocsLayout
        tree={tree}
        tabs={false}
        slots={readerSlots}
        sidebar={{ defaultOpenLevel: 0 }}
        nav={{
          title: <span>{SITE.name} <span className="docs-wordmark">docs</span></span>,
          url: "/overview",
        }}
        links={DOCS_NAV.map((item) =>
          "external" in item && item.external
            ? { type: "main", text: item.label, url: item.href, external: true }
            : { type: "main", text: item.label, url: item.href },
        )}
      >
        <DocsPage
          {...page}
          tabIndex={-1}
          breadcrumb={{ role: "navigation", "aria-label": "Breadcrumb", className: "docs-crumb" }}
          tableOfContent={{ enabled: !isOverview }}
          footer={{ enabled: !isOverview }}
        >
          <header className="docs-article-header">
            <DocsTitle>{title}</DocsTitle>
            <DocsDescription>{summary || description}</DocsDescription>
          </header>
          {isOverview ? children : <DocsBody>{children}</DocsBody>}
          {!isOverview && <CodeControls pathname={pathname} />}
          {!isOverview && (
            <footer className="docs-provenance" aria-label="Document provenance">
              {sourceUrl && <a href={sourceUrl} target="_blank" rel="noopener noreferrer">View exact source <span aria-hidden="true">↗</span></a>}
              <span>{stability} document</span>
              {isProtocol && protocolVersion && <span>Wire {protocolVersion}</span>}
            </footer>
          )}
        </DocsPage>
      </DocsLayout>
    </RootProvider>
  );
}
