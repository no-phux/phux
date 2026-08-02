import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

import { shouldActivatePhuxPackage } from "../src/activation.js";
import { registerPhuxExtension } from "../src/extension.js";

const packageRoot = dirname(dirname(fileURLToPath(import.meta.url)));

export default function phuxExtension(pi: ExtensionAPI): void {
  if (!shouldActivatePhuxPackage(packageRoot)) return;
  registerPhuxExtension(pi);
}
