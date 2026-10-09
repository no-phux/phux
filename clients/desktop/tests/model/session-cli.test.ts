import { describe, expect, test } from "bun:test";
import { createNamedSession, renameNamedSession, type CommandResult } from "../../scripts/server";
import {
  createSessionArgs,
  createSessionError,
  renameSessionArgs,
  renameSessionError,
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
