export const COCKPIT_IMPORT_BASELINE_VERSION = "0.16.1";

export function recoveryFor(tag) {
  if (tag === "next") return { workflow: "next-release.yml", extraArgs: "" };
  return { workflow: "publish.yml", extraArgs: "" };
}

export function isCockpitImportBaseline({ path, version, bootstrapSha, historyTip }) {
  return (
    path === "clients/cockpit" &&
    version === COCKPIT_IMPORT_BASELINE_VERSION &&
    bootstrapSha !== undefined &&
    bootstrapSha === historyTip
  );
}

export function isUnreleasedDesktop({ path, version, initialVersion }) {
  return path === "clients/desktop" && version === "0.0.0" && initialVersion === "0.1.0-alpha.1";
}

export function desktopReleaseProblems(release) {
  if (release.draft || !release.tag.startsWith("desktop-v")) return [];
  const version = release.tag.slice("desktop-v".length);
  const problems = [];
  if (!release.prerelease) problems.push(`${release.tag} must be marked prerelease`);
  for (const asset of [`phux-desktop-${version}-macos-arm64.zip`, "SHA256SUMS"]) {
    if (!release.assetNames.includes(asset)) problems.push(`${release.tag} is missing ${asset}`);
  }
  return problems;
}
