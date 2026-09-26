import { spawnSync } from "node:child_process";

const result = spawnSync(
  "tsc",
  ["--noEmit", "--runExternalCode", "--pretty", "false"],
  { encoding: "utf8" },
);
const output = `${result.stdout ?? ""}${result.stderr ?? ""}`;
const diagnostics = output.split("\n").filter((line) => line.includes("error TS"));
const ours = diagnostics.filter((line) => !line.includes("node_modules/"));

if (ours.length > 0) {
  console.error(ours.join("\n"));
  process.exit(1);
}

if (diagnostics.length === 0 && result.status !== 0) {
  process.stderr.write(output);
  process.exit(result.status ?? 1);
}
