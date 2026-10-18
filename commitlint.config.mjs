// Commitlint contract for phux, enforced on every PR by
// .github/workflows/commitlint.yml (all commits AND the PR title).
//
// release-please computes the next version and the changelog from this
// commit log (see docs/RELEASING.md), so a malformed message either drops
// work from the release notes or misses a version bump entirely.
//
// Deviations from @commitlint/config-conventional, matched to house style:
//   * header-max-length 120 (error): scoped subjects here legitimately run
//     long ("feat(server): two-hop attach relay with lease-aliasing fix...").
//   * body/footer line length: unlimited. Commit bodies in this repo are
//     prose paragraphs with long URLs and bead references; hard-wrapping
//     them at 100 chars is churn with no reader benefit.
export default {
  extends: ['@commitlint/config-conventional'],
  rules: {
    'header-max-length': [2, 'always', 120],
    'body-max-line-length': [0, 'always', Infinity],
    'footer-max-line-length': [0, 'always', Infinity],
  },
};
