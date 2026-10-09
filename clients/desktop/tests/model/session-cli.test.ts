import { describe, expect, test } from "bun:test";
import {
  closeNamedSession,
  createNamedSession,
  renameNamedSession,
  type CommandResult,
} from "../../scripts/server";
import { directoryForTerminal } from "../../src/path-picker";
import {
  closeSessionArgs,
  createSessionArgs,
  createSessionError,
  renameSessionArgs,
  renameSessionError,
  windowSession,
} from "../../src/workspace/session-cli";

describe("desktop session commands", () => {
  test("create names the window socket and the chosen directory", () => {
    expect(createSessionArgs("/tmp/phux.sock", "api", "/src/api")).toEqual([
      "--socket",
      "/tmp/phux.sock",
      "new",
      "--json",
      "-s",
      "api",
      "--cwd",
      "/src/api",
    ]);
    expect(createSessionError("api", "/src/api")).toBeUndefined();
    expect(createSessionError("  ", "/src/api")).toBe("A session needs a name.");
    expect(createSessionError("api", "  ")).toBe(
      "A new session needs the focused directory or a chosen folder.",
    );
  });

  test("rename names the window socket and both session names", () => {
    expect(renameSessionArgs("/tmp/phux.sock", "api", "notes")).toEqual([
      "--socket",
      "/tmp/phux.sock",
      "rename",
      "api",
      "notes",
    ]);
    expect(renameSessionError("api", "notes")).toBeUndefined();
    expect(renameSessionError("", "notes")).toBe(
      "Rename needs the current session and a new name.",
    );
  });

  test("close kills that session on the window socket", () => {
    expect(closeSessionArgs("/tmp/phux.sock", "api")).toEqual([
      "--socket",
      "/tmp/phux.sock",
      "kill",
      "api",
    ]);
    const calls: string[][] = [];
    closeNamedSession("/bin/phux", "/tmp/phux.sock", "api", (binary, args) => {
      calls.push([binary, ...args]);
      return { status: 0, stderr: "" };
    });
    expect(calls).toEqual([["/bin/phux", ...closeSessionArgs("/tmp/phux.sock", "api")]]);
    expect(() =>
      closeNamedSession("/bin/phux", "/tmp/phux.sock", "  ", () => ({ status: 0, stderr: "" })),
    ).toThrow("Close needs the session name.");
  });

  test("a new window keeps the current session unless a name is chosen", () => {
    expect(windowSession(undefined, "beta")).toBe("beta");
    expect(windowSession("  ", "beta")).toBe("beta");
    expect(windowSession("api", "beta")).toBe("api");
  });

  test("a host directory can start a terminal and a file cannot", () => {
    expect(directoryForTerminal("directory", "/src/api")).toBe("/src/api");
    expect(directoryForTerminal("file", "/src/api/main.rs")).toBeUndefined();
    expect(directoryForTerminal("directory", "  ")).toBeUndefined();
  });

  test("an empty directory never spawns phux", () => {
    const calls: string[][] = [];
    const run = (binary: string, args: string[]): CommandResult => {
      calls.push([binary, ...args]);
      return { status: 0, stderr: "" };
    };
    expect(() => createNamedSession("/bin/phux", "/tmp/phux.sock", "api", "  ", run)).toThrow(
      "A new session needs the focused directory or a chosen folder.",
    );
    expect(calls).toEqual([]);
  });

  test("create and rename spawn the socket-scoped arguments", () => {
    const calls: string[][] = [];
    const run = (binary: string, args: string[]): CommandResult => {
      calls.push([binary, ...args]);
      return { status: 0, stderr: "" };
    };
    createNamedSession("/bin/phux", "/tmp/phux.sock", "api", "/src/api", run);
    renameNamedSession("/bin/phux", "/tmp/phux.sock", "api", "notes", run);
    expect(calls).toEqual([
      ["/bin/phux", ...createSessionArgs("/tmp/phux.sock", "api", "/src/api")],
      ["/bin/phux", ...renameSessionArgs("/tmp/phux.sock", "api", "notes")],
    ]);
    expect(() =>
      renameNamedSession("/bin/phux", "/tmp/phux.sock", "api", "notes", () => ({
        status: 2,
        stderr: "already exists",
      })),
    ).toThrow("already exists");
  });
});
