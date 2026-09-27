import { describe, expect, test } from "bun:test";
import {
  acceptResult,
  beginPicker,
  canOfferPathPicker,
  displayPath,
  hostForTerminal,
  preparedInsertion,
  sameTarget,
  shellQuotePath,
  startQuery,
} from "../../src/path-picker";

const target = { terminalId: "local:1", placementId: "place-1", serverId: "a", epoch: "3" };

describe("host path picker", () => {
  test("old peers and unattached clients cannot offer the action", () => {
    expect(canOfferPathPicker(undefined, true, target)).toBe(false);
    expect(canOfferPathPicker([], true, target)).toBe(false);
    expect(canOfferPathPicker(["path-query"], false, target)).toBe(false);
    expect(canOfferPathPicker(["path-query"], true, undefined)).toBe(false);
    expect(canOfferPathPicker(["path-query"], true, target)).toBe(true);
  });
  test("newer search, cancellation and late replies cannot replace the current listing", () => {
    const browsing = startQuery(beginPicker(target, "~"), 9, "");
    const searching = startQuery(browsing, 10, "main");
    const stale = {
      requestId: 9,
      root: "/host",
      rows: [{ path: "/host/old", kind: "file" as const }],
    };
    expect(acceptResult(searching, stale)).toEqual(searching);
    const found = acceptResult(searching, {
      requestId: 10,
      root: "/host",
      rows: [{ path: "/host/main.rs", kind: "file" }],
    });
    expect(found?.rows[0]?.path).toBe("/host/main.rs");
    expect(acceptResult(undefined, stale)).toBeUndefined();
  });

  test("a replaced placement, server or epoch is never a valid insertion target", () => {
    expect(sameTarget(target, { ...target })).toBe(true);
    expect(sameTarget(target, { ...target, placementId: "place-2" })).toBe(false);
    expect(sameTarget(target, { ...target, epoch: "4" })).toBe(false);
    expect(sameTarget(target, { ...target, serverId: "b" })).toBe(false);
    expect(sameTarget(target, undefined)).toBe(false);
  });

  test("quoting makes spaces, quotes and shell substitutions inert without Enter", () => {
    expect(shellQuotePath("/host/a b/it's $(bad);`bad`")).toBe(
      `'/host/a b/it'"'"'s $(bad);\`bad\`'`,
    );
    expect(shellQuotePath("/host/empty")).toBe("'/host/empty'");
    expect(shellQuotePath("/host/evil\ncommand")).toBeUndefined();
    expect(shellQuotePath("/host/evil\x1b[2J")).toBeUndefined();
    expect(shellQuotePath("relative")).toBeUndefined();
  });

  test("insertion requires a returned row, same target, readiness and no pending search", () => {
    const found = acceptResult(startQuery(beginPicker(target, "~"), 2, "x"), {
      requestId: 2,
      root: "/host",
      rows: [{ path: "/host/it's here", kind: "file" }],
    });
    expect(preparedInsertion(found, "/host/it's here", target, true)).toBe(`'/host/it'"'"'s here'`);
    expect(preparedInsertion(found, "/host/other", target, true)).toBeUndefined();
    expect(
      preparedInsertion(found, "/host/it's here", { ...target, epoch: "4" }, true),
    ).toBeUndefined();
    expect(preparedInsertion(found, "/host/it's here", target, false)).toBeUndefined();
    expect(
      preparedInsertion(startQuery(found!, 3, "new"), "/host/it's here", target, true),
    ).toBeUndefined();
  });

  test("host text is shown with controls escaped, never raw", () => {
    expect(displayPath("/host/plain name")).toBe("/host/plain name");
    expect(displayPath("/host/x\x1b]0;pwn\x07\ny")).toBe("/host/x\\u{1b}]0;pwn\\u{7}\\u{a}y");
    expect(displayPath("/host/\u009b2J")).toBe("/host/\\u{9b}2J");
  });

  test("satellite terminal ids route to their host, including hosts containing colons", () => {
    expect(hostForTerminal("local:1")).toBeUndefined();
    expect(hostForTerminal("satellite:box:2222:3")).toBe("box:2222");
    expect(hostForTerminal("satellite::3")).toBeUndefined();
  });
});
