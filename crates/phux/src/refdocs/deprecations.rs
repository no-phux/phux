//! The generated deprecations reference. No spelling is currently
//! deprecated; the page states the policy a future deprecation follows.

use super::Page;

/// Render `docs/reference/deprecations.md`.
pub(crate) fn page() -> Page {
    let body = String::from(
        "No spelling is currently deprecated.\n\n\
         When one is, it keeps parsing with its full argument surface and \
         runs its replacement's implementation, with three differences: one \
         warning line on stderr naming the replacement (suppressed under \
         `--json`), absence from `--help`, and absence from the generated \
         shell completions. A deprecated spelling survives at least one full \
         release cycle with the warning in place before it is removed.\n",
    );

    Page {
        file: "deprecations.md",
        title: "phux deprecations reference",
        summary: "Every deprecated spelling, its replacement, and its \
                  removal release.",
        tldr: "Deprecated spellings the current binary still accepts, \
               each pinned with its replacement and lifecycle releases; \
               empty when nothing is currently deprecated.",
        body,
    }
}
