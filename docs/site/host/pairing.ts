/**
 * Universal Link support for phux pairing (phux-z84.4).
 *
 * `phux pair --qr` encodes an https link on this site's host, not the
 * `phux://` form, and deliberately so: a custom URL scheme is not exclusive
 * on iOS, so any installed app may register `phux` and collect the bearer
 * token at the moment the person scans. An https Universal Link cannot be
 * claimed that way, because the OS resolves it against the
 * apple-app-site-association this file serves, which only the domain's owner
 * can publish.
 *
 * That makes this file load-bearing rather than decorative: without it iOS
 * has no association, the QR opens Safari instead of the app, and pairing by
 * camera fails with no diagnostic anywhere. It is served from the worker and
 * not from dist/ so the content type is guaranteed — Apple requires
 * `application/json`, and an extensionless static asset does not reliably get
 * one.
 *
 * Apple fetches this over TLS with no redirects permitted. Any 3xx on the
 * declared domain is the same as having no association at all.
 */

/** The App Store team and bundle id, as `TEAMID.bundle.id`. */
export const APP_ID = "AE44G4MFLU.dev.phux.mobile";

/** The path Apple fetches. The legacy root path is served too (see below). */
export const AASA_PATH = "/.well-known/apple-app-site-association";

/** The pre-iOS-13 location, still fetched by some OS versions. */
export const AASA_LEGACY_PATH = "/apple-app-site-association";

/**
 * Paths the app claims. Only the pairing link — claiming a domain routes
 * *every* matching https URL into the app, so the marketing pages, the docs
 * and the install funnel must stay in the browser. `?` and `*` are Apple's
 * wildcards; `/connect` alone would not match the query-bearing real link.
 */
export const CLAIMED_PATHS = ["/connect", "/connect?*"] as const;

export interface AppleAppSiteAssociation {
  applinks: {
    details: Array<{ appIDs: string[]; components: Array<{ "/": string; comment: string }> }>;
  };
}

export function associationDocument(): AppleAppSiteAssociation {
  return {
    applinks: {
      details: [
        {
          appIDs: [APP_ID],
          components: CLAIMED_PATHS.map((path) => ({
            "/": path,
            comment: "phux pairing link from `phux pair --qr`",
          })),
        },
      ],
    },
  };
}

export function isAssociationPath(pathname: string): boolean {
  return pathname === AASA_PATH || pathname === AASA_LEGACY_PATH;
}

/**
 * The association response, or null when the path is not Apple's.
 *
 * Cached briefly rather than forever: the association is fetched on install
 * and refreshed periodically, and a long TTL turns a bad appID into a
 * multi-day outage with no way to push a correction.
 */
export function handleAssociationRequest(url: URL): Response | null {
  if (!isAssociationPath(url.pathname)) return null;
  return new Response(JSON.stringify(associationDocument(), null, 2), {
    status: 200,
    headers: {
      "content-type": "application/json",
      "cache-control": "public, max-age=300",
      // Apple's fetcher ignores CORS, but the /connect page reads this to
      // tell the visitor whether the association is live.
      "access-control-allow-origin": "*",
    },
  });
}
