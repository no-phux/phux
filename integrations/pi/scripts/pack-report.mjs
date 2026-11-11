// `npm pack --json` prints an array through npm 11 and an object keyed by
// package name from npm 12 on; the release workflow runs npm 12.
export function packReport(stdout) {
  const report = JSON.parse(stdout);
  return Array.isArray(report) ? report : Object.values(report);
}
