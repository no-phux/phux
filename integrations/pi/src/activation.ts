import { readFileSync, realpathSync } from "node:fs";
import { join, resolve } from "node:path";

import {
  DefaultPackageManager,
  SettingsManager,
  getAgentDir,
} from "@earendil-works/pi-coding-agent";

const PHUX_PI_PACKAGE_NAME = "@phux/pi";

export interface ConfiguredPiPackage {
  readonly scope: "user" | "project";
  readonly installedPath?: string;
}

/**
 * Let the first globally configured @phux/pi package win over an auto-loaded
 * project checkout. Pi otherwise treats two local checkouts as unrelated
 * package identities and reports every shared tool as a conflict.
 */
export function shouldActivatePhuxPackage(
  currentPackageRoot: string,
  configuredPackages: readonly ConfiguredPiPackage[] = configuredPiPackages(),
): boolean {
  const preferredGlobal = configuredPackages.find((entry) =>
    entry.scope === "user" &&
    entry.installedPath !== undefined &&
    packageName(entry.installedPath) === PHUX_PI_PACKAGE_NAME);

  return preferredGlobal?.installedPath === undefined ||
    canonicalPath(preferredGlobal.installedPath) === canonicalPath(currentPackageRoot);
}

function configuredPiPackages(): readonly ConfiguredPiPackage[] {
  try {
    const agentDir = getAgentDir();
    const settingsManager = SettingsManager.create(process.cwd(), agentDir);
    return new DefaultPackageManager({
      cwd: process.cwd(),
      agentDir,
      settingsManager,
    }).listConfiguredPackages();
  } catch {
    // Package arbitration must never make an otherwise usable extension fail to load.
    return [];
  }
}

function packageName(root: string): string | undefined {
  try {
    const parsed = JSON.parse(readFileSync(join(root, "package.json"), "utf8")) as { name?: unknown };
    return typeof parsed.name === "string" ? parsed.name : undefined;
  } catch {
    return undefined;
  }
}

function canonicalPath(path: string): string {
  try {
    return realpathSync(path);
  } catch {
    return resolve(path);
  }
}
