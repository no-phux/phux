#!/usr/bin/env node
import { PhuxCli } from "@phux/integration-runtime/adapter";

import { runChief } from "./run.js";
import { chiefPaths, loadConfig } from "./config.js";
import { ensure, supervise } from "./ensure.js";

const USAGE = `phux-chief — the phux chief of staff (docs/chief.md)

  phux-chief run        run the chief in this pane (raw-mode input)
  phux-chief supervise  run it, restarting on exit (what the chief pane runs)
  phux-chief ensure     start the chief pane on the chief host if it is missing
  phux-chief config     print the effective configuration and paths`;

const [command = "run", ...rest] = process.argv.slice(2);
const cli = () => new PhuxCli({ executable: process.env.PHUX_BIN || "phux", ...(process.env.PHUX_SOCKET ? { socket: process.env.PHUX_SOCKET } : {}), env: process.env });

switch (command) {
  case "run":
    await runChief();
    process.exit(0);
    break;
  case "supervise":
    await supervise(rest);
    break;
  case "ensure": {
    const outcome = await ensure(cli());
    process.stdout.write(`${JSON.stringify(outcome)}\n`);
    break;
  }
  case "config": {
    const paths = chiefPaths();
    process.stdout.write(`${JSON.stringify({ paths, config: loadConfig(paths) }, null, 2)}\n`);
    break;
  }
  default:
    process.stderr.write(`${USAGE}\n`);
    process.exit(command === "help" || command === "--help" ? 0 : 2);
}
