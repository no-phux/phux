import { afterEach, beforeEach, expect, spyOn, test } from "bun:test";
import { parseArgs } from "node:util";
import { Failure, invoke, required, UsageError, type Command } from "./command.ts";

let printed: string[];
let spies: { mockRestore(): void }[];

beforeEach(() => {
  printed = [];
  const record = (chunk: unknown): boolean => {
    printed.push(String(chunk));
    return true;
  };
  spies = [
    spyOn(console, "log").mockImplementation(record),
    spyOn(console, "error").mockImplementation(record),
    spyOn(process.stderr, "write").mockImplementation(record),
  ];
});

afterEach(() => {
  for (const spy of spies) spy.mockRestore();
});

const flagged: Command = {
  summary: "Takes one flag.",
  usage: "--name NAME",
  help: "More words.",
  run(args) {
    const { values } = parseArgs({ args, options: { name: { type: "string" } } });
    if (values.name === "boom") throw new Failure("it broke");
    if (values.name === "bug") throw new TypeError("a bug");
    return required(values.name, "--name") === "ok" ? 0 : 1;
  },
};

test("a leading --help prints usage, summary and help without running", async () => {
  expect(await invoke("flagged", flagged, ["--help"])).toBe(0);
  expect(printed.join("\n")).toBe(
    "usage: bun tools/main.ts flagged --name NAME\n\nTakes one flag.\nMore words.",
  );
});

test("a bad command line exits 2 with the usage", async () => {
  for (const args of [["--nope"], [], ["--name"]]) {
    printed = [];
    expect(await invoke("flagged", flagged, args)).toBe(2);
    expect(printed.join("")).toStartWith("usage: bun tools/main.ts flagged --name NAME\n");
    expect(printed.join("")).toContain("flagged: error: ");
  }
});

test("a command without a usage refuses arguments", async () => {
  const bare: Command = { summary: "None.", run: () => 0 };
  expect(await invoke("bare", bare, [])).toBe(0);
  expect(await invoke("bare", bare, ["extra"])).toBe(2);
  expect(printed.join("")).toContain("bare: error: takes no arguments, got extra");
});

test("a Failure exits 1 with its message; anything else propagates", async () => {
  expect(await invoke("flagged", flagged, ["--name", "boom"])).toBe(1);
  expect(printed.join("")).toContain("error:");
  expect(printed.join("")).toContain("it broke");
  let thrown: unknown;
  await invoke("flagged", flagged, ["--name", "bug"]).catch((error: unknown) => {
    thrown = error;
  });
  expect(thrown).toBeInstanceOf(TypeError);
});

test("UsageError is a usage failure, not a Failure", () => {
  expect(new UsageError("x")).not.toBeInstanceOf(Failure);
});
