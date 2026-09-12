export const COCKPIT_IMPORT_BASELINE_VERSION = "0.16.1";

export function recoveryFor(tag) {
  if (tag === "next") return { workflow: "next-release.yml", extraArgs: "" };
  if (tag.startsWith("v")) return { workflow: "release.yml", extraArgs: "" };
  if (tag.startsWith("cockpit-v")) return { workflow: "cockpit-release.yml", extraArgs: "" };
  return { workflow: "agent-integration-release.yml", extraArgs: " -f dry_run=false" };
}

export function isCockpitImportBaseline({ path, version, bootstrapSha, historyTip }) {
  return (
    path === "clients/cockpit" &&
    version === COCKPIT_IMPORT_BASELINE_VERSION &&
    bootstrapSha !== undefined &&
    bootstrapSha === historyTip
  );
}
