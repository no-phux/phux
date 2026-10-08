import { describe, expect, test } from "bun:test";
import {
  DOCS_HOST,
  SITE_HOST,
  isDocsHost,
  isDocsPath,
  isMarketingOnlyPath,
  routeRequest,
} from "./routes";

function urlOn(host: string, path: string): URL {
  return new URL(path, `https://${host}`);
}

describe("docs host detection", () => {
  test("matches the production docs hostname", () => {
    expect(isDocsHost(DOCS_HOST)).toBe(true);
    expect(isDocsHost("docs.phux.sh:443")).toBe(true);
    expect(isDocsHost("docs.preview.example")).toBe(true);
  });

  test("does not treat the marketing host or workers.dev as docs", () => {
    expect(isDocsHost(SITE_HOST)).toBe(false);
    expect(isDocsHost("www.phux.sh")).toBe(false);
    expect(isDocsHost("phux-site.account.workers.dev")).toBe(false);
    expect(isDocsHost("localhost")).toBe(false);
  });
});

describe("path classification", () => {
  test("docs prefixes include overview, guides, and search", () => {
    expect(isDocsPath("/overview")).toBe(true);
    expect(isDocsPath("/quickstart/install")).toBe(true);
    expect(isDocsPath("/wire/l1")).toBe(true);
    expect(isDocsPath("/api/search.json")).toBe(true);
    expect(isDocsPath("/consumers/cockpit")).toBe(true);
  });

  test("marketing paths stay off the docs host", () => {
    expect(isDocsPath("/")).toBe(false);
    expect(isDocsPath("/embed")).toBe(false);
    expect(isDocsPath("/install")).toBe(false);
    expect(isDocsPath("/install-cockpit.sh")).toBe(false);
    expect(isDocsPath("/og.png")).toBe(false);
    expect(isMarketingOnlyPath("/embed")).toBe(true);
    expect(isMarketingOnlyPath("/install.sh")).toBe(true);
    expect(isMarketingOnlyPath("/quickstart")).toBe(false);
  });
});

describe("host routing", () => {
  test("docs.phux.sh/ becomes /overview", () => {
    expect(routeRequest(DOCS_HOST, urlOn(DOCS_HOST, "/"))).toEqual({
      kind: "redirect",
      location: "https://docs.phux.sh/overview",
      status: 301,
    });
  });

  test("docs.phux.sh serves the docs tree as assets", () => {
    expect(routeRequest(DOCS_HOST, urlOn(DOCS_HOST, "/overview"))).toEqual({ kind: "asset" });
    expect(routeRequest(DOCS_HOST, urlOn(DOCS_HOST, "/wire/l1"))).toEqual({ kind: "asset" });
  });

  test("docs.phux.sh sends installers and embed back to phux.sh", () => {
    expect(routeRequest(DOCS_HOST, urlOn(DOCS_HOST, "/install"))).toEqual({
      kind: "redirect",
      location: "https://phux.sh/install",
      status: 301,
    });
    expect(routeRequest(DOCS_HOST, urlOn(DOCS_HOST, "/embed"))).toEqual({
      kind: "redirect",
      location: "https://phux.sh/embed",
      status: 301,
    });
  });

  test("demo pages and media redirect only within the demos path boundary", () => {
    for (const path of [
      "/demos",
      "/demos/",
      "/demos.html",
      "/demos/index.html",
      "/demos/program-status-osc7501?from=docs",
      "/demos/program-status-osc7501.html",
      "/demos/program-status-osc7501/video.mp4",
      "/demos/program-status-osc7501/poster.webp",
      "/demos/program-status-osc7501/captions.vtt",
      "/demos/program-status-osc7501/session.cast",
    ]) {
      expect(isMarketingOnlyPath(urlOn(DOCS_HOST, path).pathname)).toBe(true);
      expect(isDocsPath(urlOn(SITE_HOST, path).pathname)).toBe(false);
      expect(routeRequest(DOCS_HOST, urlOn(DOCS_HOST, path))).toEqual({
        kind: "redirect",
        location: `https://${SITE_HOST}${path}`,
        status: 301,
      });
      expect(routeRequest(SITE_HOST, urlOn(SITE_HOST, path))).toEqual({ kind: "asset" });
      expect(routeRequest("phux-site.account.workers.dev", urlOn(SITE_HOST, path))).toEqual({ kind: "asset" });
    }
    for (const path of ["/demosphere", "/demos-old/video.mp4", "/demos.mp4"]) {
      expect(isMarketingOnlyPath(path)).toBe(false);
      expect(routeRequest(DOCS_HOST, urlOn(DOCS_HOST, path))).toEqual({ kind: "asset" });
    }
  });

  test("phux.sh 301s docs paths onto docs.phux.sh", () => {
    expect(routeRequest(SITE_HOST, urlOn(SITE_HOST, "/docs"))).toEqual({
      kind: "redirect",
      location: "https://docs.phux.sh/docs",
      status: 301,
    });
    expect(routeRequest(SITE_HOST, urlOn(SITE_HOST, "/quickstart?from=hero"))).toEqual({
      kind: "redirect",
      location: "https://docs.phux.sh/quickstart?from=hero",
      status: 301,
    });
    expect(routeRequest(SITE_HOST, urlOn(SITE_HOST, "/wire/l1"))).toEqual({
      kind: "redirect",
      location: "https://docs.phux.sh/wire/l1",
      status: 301,
    });
    expect(routeRequest(`www.${SITE_HOST}`, urlOn(`www.${SITE_HOST}`, "/docs"))).toEqual({
      kind: "redirect",
      location: "https://docs.phux.sh/docs",
      status: 301,
    });
  });

  test("evaluation and recovery pages use the docs origin without swallowing similarly named assets", () => {
    expect(routeRequest(SITE_HOST, urlOn(SITE_HOST, "/performance?view=latency"))).toEqual({
      kind: "redirect",
      location: "https://docs.phux.sh/performance?view=latency",
      status: 301,
    });
    expect(routeRequest(SITE_HOST, urlOn(SITE_HOST, "/troubleshooting"))).toEqual({
      kind: "redirect",
      location: "https://docs.phux.sh/troubleshooting",
      status: 301,
    });
    expect(routeRequest(SITE_HOST, urlOn(SITE_HOST, "/performance-chart.svg"))).toEqual({ kind: "asset" });
  });

  test("phux.sh keeps the landing, installers, and embed", () => {
    expect(routeRequest(SITE_HOST, urlOn(SITE_HOST, "/"))).toEqual({ kind: "asset" });
    expect(routeRequest(SITE_HOST, urlOn(SITE_HOST, "/install"))).toEqual({ kind: "asset" });
    expect(routeRequest(SITE_HOST, urlOn(SITE_HOST, "/embed"))).toEqual({ kind: "asset" });
    expect(routeRequest(SITE_HOST, urlOn(SITE_HOST, "/og.png"))).toEqual({ kind: "asset" });
  });

  test("workers.dev preview serves every path so a single deploy can be reviewed", () => {
    const preview = "phux-site.account.workers.dev";
    expect(routeRequest(preview, urlOn(preview, "/"))).toEqual({ kind: "asset" });
    expect(routeRequest(preview, urlOn(preview, "/overview"))).toEqual({ kind: "asset" });
    expect(routeRequest(preview, urlOn(preview, "/quickstart"))).toEqual({ kind: "asset" });
  });
});
