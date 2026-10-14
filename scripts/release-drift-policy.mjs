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

// The desktop train graduated out of `-alpha.N` prereleases: those published
// tags are a retired channel, not stuck stable releases, and nothing resolves
// or serves them any more. Only a stable desktop-vX.Y.Z carries the install
// contract the installer and the download page follow.
const DESKTOP_STABLE_TAG = /^desktop-v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

export function desktopReleaseProblems(release) {
  if (release.draft || !DESKTOP_STABLE_TAG.test(release.tag)) return [];
  const version = release.tag.slice("desktop-v".length);
  const problems = [];
  if (release.prerelease) problems.push(`${release.tag} must not be marked prerelease`);
  for (const asset of [`phux-desktop-${version}-macos-arm64.zip`, "SHA256SUMS"]) {
    if (!release.assetNames.includes(asset)) problems.push(`${release.tag} is missing ${asset}`);
  }
  return problems;
}
