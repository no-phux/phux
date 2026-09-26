import { describe, expect, test } from "bun:test";
import {
  AASA_LEGACY_PATH,
  AASA_PATH,
  APP_ID,
  associationDocument,
  handleAssociationRequest,
  isAssociationPath,
} from "./pairing";
import { DOCS_HOST, SITE_HOST, routeRequest } from "./routes";

function urlOn(host: string, path: string): URL {
  return new URL(path, `https://${host}`);
}

describe("association document", () => {
  test("names the shipping app id as TEAMID.bundleid", () => {
    expect(APP_ID).toBe("AE44G4MFLU.dev.phux.mobile");
    expect(APP_ID.split(".").length).toBeGreaterThanOrEqual(3);
    expect(associationDocument().applinks.details[0]?.appIDs).toEqual([APP_ID]);
  });

  // The real link is `/connect?url=…&token=…`. A bare "/connect" component
  // does not match a query-bearing URL, which is the mistake that looks
  // correct in review and fails only on a device.
  test("claims the query-bearing pairing link, not just the bare path", () => {
    const components = associationDocument().applinks.details[0]?.components ?? [];
    const paths = components.map((c) => c["/"]);
    expect(paths).toContain("/connect");
    expect(paths).toContain("/connect?*");
  });

  // Claiming a domain routes every matching https URL into the app. The
  // marketing pages, docs and install funnel must stay in the browser.
  test("claims nothing outside the pairing path", () => {
    const paths = (associationDocument().applinks.details[0]?.components ?? []).map((c) => c["/"]);
    for (const path of paths) expect(path.startsWith("/connect")).toBe(true);
    expect(paths).not.toContain("*");
    expect(paths).not.toContain("/*");
  });
});

describe("association route", () => {
  test("answers both the well-known and legacy root paths", () => {
    expect(isAssociationPath(AASA_PATH)).toBe(true);
    expect(isAssociationPath(AASA_LEGACY_PATH)).toBe(true);
    expect(isAssociationPath("/connect")).toBe(false);
    expect(isAssociationPath("/.well-known/ai-catalog.json")).toBe(false);
  });

  test("serves application/json, which Apple requires", async () => {
    const response = handleAssociationRequest(urlOn(SITE_HOST, AASA_PATH));
    expect(response).not.toBeNull();
    expect(response?.status).toBe(200);
    expect(response?.headers.get("content-type")).toBe("application/json");
    expect(JSON.parse(await response!.text())).toEqual(associationDocument());
  });

  test("is not a candidate for any host redirect", () => {
    // Apple follows no redirects: a 3xx here is the same as no association.
    // Pinned on both hosts so a future DOCS_PREFIXES entry cannot capture it.
    for (const host of [SITE_HOST, DOCS_HOST, `www.${SITE_HOST}`]) {
      expect(routeRequest(host, urlOn(host, AASA_PATH)).kind).toBe("asset");
    }
  });

  test("declines paths that are not the association", () => {
    expect(handleAssociationRequest(urlOn(SITE_HOST, "/connect"))).toBeNull();
    expect(handleAssociationRequest(urlOn(SITE_HOST, "/"))).toBeNull();
  });
});
