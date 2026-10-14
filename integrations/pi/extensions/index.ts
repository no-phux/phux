import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { shouldActivatePhuxPackage } from "../src/activation.js";

const packageRoot = dirname(dirname(fileURLToPath(import.meta.url)));

export default async function phuxExtension(pi: ExtensionAPI): Promise<void> {
  if (!shouldActivatePhuxPackage(packageRoot)) return;
  // An inactive source checkout need not have its runtime dependencies installed.
  const { registerPhuxExtension } = await import("../src/extension.js");
  registerPhuxExtension(pi);
}
