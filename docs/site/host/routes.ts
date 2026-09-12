/**
 * Host split for the static site worker.
 *
 * phux.sh is the product surface (landing, installers, live demo).
 * docs.phux.sh is the documentation surface (Fumadocs tree + /overview).
 *
 * Pure functions so unit tests do not need a Cloudflare runtime.
 */

export const SITE_HOST = "phux.sh";
export const DOCS_HOST = "docs.phux.sh";

/** Paths that belong on docs.phux.sh. Prefix match, including the exact path. */
export const DOCS_PREFIXES = [
  "/overview",
  "/docs",
  "/quickstart",
  "/concepts",
  "/consumers",
  "/remote-access",
  "/reference",
  "/wire",
  "/architecture",
  "/decisions",
  "/api/search",
  "/api/search.json",
] as const;

export type HostRoute =
  | { kind: "asset" }
  | { kind: "redirect"; location: string; status: 301 };

export function hostnameOf(host: string): string {
  return host.toLowerCase().split(":")[0] ?? host;
}

export function isDocsHost(host: string): boolean {
  const hostname = hostnameOf(host);
  return hostname === DOCS_HOST || hostname.startsWith("docs.");
}

export function isSiteHost(host: string): boolean {
  const hostname = hostnameOf(host);
  return hostname === SITE_HOST || hostname === `www.${SITE_HOST}`;
}

export function isDocsPath(pathname: string): boolean {
  const path = stripHtmlSuffix(pathname);
  return DOCS_PREFIXES.some((prefix) => path === prefix || path.startsWith(`${prefix}/`));
}

export function isMarketingOnlyPath(pathname: string): boolean {
  const path = stripHtmlSuffix(pathname);
  return path === "/embed" || path.startsWith("/embed/") || path === "/install" || path.startsWith("/install");
}

export function routeRequest(host: string, url: URL): HostRoute {
  const path = url.pathname;

  if (isDocsHost(host)) {
    if (path === "/" || path === "/index.html" || path === "") {
      return redirectOn(DOCS_HOST, "/overview", url.search);
    }
    if (isMarketingOnlyPath(path)) {
      return redirectOn(SITE_HOST, path, url.search);
    }
    return { kind: "asset" };
  }

  // Only the production marketing host bounces docs paths. Preview
  // (workers.dev) and local wrangler serve the whole tree so a single
  // deploy can be reviewed.
  if (isSiteHost(host) && isDocsPath(path)) {
    return redirectOn(DOCS_HOST, path, url.search);
  }
  return { kind: "asset" };
}

function redirectOn(host: string, path: string, search: string): HostRoute {
  const location = new URL(path, `https://${host}`);
  location.search = search;
  return { kind: "redirect", location: location.href, status: 301 };
}

function stripHtmlSuffix(pathname: string): string {
  if (pathname.endsWith("/index.html")) {
    const trimmed = pathname.slice(0, -"/index.html".length);
    return trimmed === "" ? "/" : trimmed;
  }
  if (pathname.endsWith(".html")) return pathname.slice(0, -".html".length);
  return pathname;
}
