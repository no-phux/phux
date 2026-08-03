//! Generated reference documentation: the page registry and renderer behind
//! the hidden `phux gen-reference-docs` subcommand and the freshness test.
//!
//! Every page under `docs/reference/` is a pure function of the compiled
//! binary — the clap command tree today, further inventories (config schema,
//! actions, widgets, hooks) as they register here. The contract has three
//! legs:
//!
//! 1. [`pages`] is the single registry. The generator writes exactly these
//!    pages; the freshness test compares exactly these pages; the index page
//!    lists exactly these pages. Registering a renderer here is the ONLY way
//!    a file legitimately appears under `docs/reference/`.
//! 2. Rendering is deterministic. No timestamps, no environment reads, no
//!    terminal-width probes (the workspace's clap has no `wrap_help`
//!    feature, so help text never wraps to the terminal). Running the
//!    generator twice yields identical bytes, which is what lets a unit test
//!    byte-compare the checked-in tree against a fresh render.
//! 3. Every page carries the doc-system scaffolding (frontmatter, TL;DR)
//!    demanded by `scripts/check-docs.sh`, so the generated tree passes
//!    `just docs-check` with no carve-outs, plus a GENERATED FILE marker so
//!    a human reader knows not to edit it.
//!
//! See `ADR/0069-generated-reference-docs.md` for why the generator is a
//! hidden subcommand rather than an xtask or a build script.

pub(crate) mod actions;
pub(crate) mod cli;
pub(crate) mod config;
pub(crate) mod exit_codes;
pub(crate) mod files;
pub(crate) mod hooks;
pub(crate) mod widgets;

/// The `last-reviewed` date stamped into every generated page's frontmatter.
///
/// Deliberately a fixed, generator-owned constant rather than "today":
/// regeneration must be byte-idempotent, and a date that moved on every run
/// would churn the tree without any content change. Bump it when a
/// generator change meaningfully alters what the pages say.
pub(crate) const GENERATED_LAST_REVIEWED: &str = "2026-08-02";

/// The reader-facing warning embedded in every generated page, right after
/// the TL;DR (the docs gate requires the TL;DR to be the first content, so
/// the marker cannot come earlier).
const GENERATED_MARKER: &str = "<!--\n\
    GENERATED FILE - do not edit. A unit test byte-compares this page\n\
    against `phux gen-reference-docs` output and fails on any drift, so\n\
    hand edits do not survive. Regenerate with `just docs-gen`.\n\
    -->";

/// One generated reference page: identity for the registry and the index,
/// plus the rendered markdown body.
pub(crate) struct Page {
    /// Filename within `docs/reference/`.
    pub(crate) file: &'static str,
    /// H1 title.
    title: &'static str,
    /// One-line description, used by the index page's table.
    summary: &'static str,
    /// TL;DR paragraph text (without the `**TL;DR.**` prefix).
    tldr: &'static str,
    /// Markdown body following the generated-file marker.
    body: String,
}

impl Page {
    /// The complete file contents: frontmatter, H1, TL;DR, generated-file
    /// marker, body — exactly what the generator writes and the freshness
    /// test expects on disk.
    pub(crate) fn render(&self) -> String {
        let Self {
            title, tldr, body, ..
        } = self;
        format!(
            "---\n\
             audience: humans, agents, contributors\n\
             stability: evolving\n\
             last-reviewed: {GENERATED_LAST_REVIEWED}\n\
             ---\n\
             \n\
             # {title}\n\
             \n\
             **TL;DR.** {tldr}\n\
             \n\
             {GENERATED_MARKER}\n\
             \n\
             {}\n",
            body.trim_end()
        )
    }
}

/// The full page registry: the index first, then every content page.
///
/// This is the extension point for further generated references: add a
/// sibling module rendering a `Page` and push it into `content` here. The
/// index table, the generator's write set, and the freshness test's compare
/// set all follow automatically.
pub(crate) fn pages() -> Vec<Page> {
    let content = vec![
        cli::page(),
        config::page(),
        actions::page(),
        widgets::page(),
        hooks::page(),
        exit_codes::page(),
        files::page(),
    ];
    let mut pages = Vec::with_capacity(content.len() + 1);
    pages.push(index_page(&content));
    pages.extend(content);
    pages
}

/// The `README.md` index over the content pages.
fn index_page(content: &[Page]) -> Page {
    use std::fmt::Write as _;

    let mut body = String::from(
        "Every file in this directory is rendered from the compiled `phux` \
         binary by `just docs-gen`; none of it is written by hand. A unit \
         test re-renders each page and byte-compares it against this tree, \
         so the reference cannot drift from the binary without failing the \
         test suite.\n\n\
         | Page | Contents |\n\
         |---|---|\n",
    );
    for page in content {
        let Page { file, summary, .. } = page;
        let _ = writeln!(body, "| [{file}]({file}) | {summary} |");
    }
    Page {
        file: "README.md",
        title: "phux reference",
        summary: "Index of the generated reference pages.",
        tldr: "Generated reference documentation, rendered from the compiled \
               binary and byte-pinned by a freshness test so it cannot drift \
               from the code. The table below routes to each page; none of \
               these files is edited by hand.",
        body,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{Page, pages};

    /// `docs/reference/` in this checkout, resolved from the crate root so
    /// the test works from any test-runner working directory.
    fn reference_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/reference")
    }

    /// The freshness gate (and the only CI wiring the generated docs have):
    /// every registered page must match its on-disk bytes, and every on-disk
    /// file must be a registered page. Hand edits, surface changes without a
    /// regeneration, and stray hand-authored files all fail here, each with
    /// `just docs-gen` as the named remedy.
    #[test]
    fn generated_reference_docs_match_the_tree() {
        let dir = reference_dir();
        let pages = pages();
        for page in &pages {
            let path = dir.join(page.file);
            let on_disk = fs::read(&path).unwrap_or_else(|err| {
                panic!(
                    "docs/reference/{} is missing or unreadable ({err}); \
                     run `just docs-gen` and commit the result",
                    page.file
                )
            });
            assert!(
                on_disk == page.render().into_bytes(),
                "docs/reference/{} does not match the generator's output \
                 (a hand edit, or a surface change without a regeneration); \
                 run `just docs-gen` and commit the result",
                page.file
            );
        }
        for entry in fs::read_dir(&dir).expect("docs/reference/ is a readable directory") {
            let name = entry.expect("readable directory entry").file_name();
            let name = name.to_string_lossy();
            assert!(
                pages.iter().any(|page| page.file == name),
                "docs/reference/{name} is not produced by refdocs::pages(); \
                 every file under docs/reference/ is generated, with no \
                 hand-authored exemptions — register a renderer for it (then \
                 run `just docs-gen`) or delete it"
            );
        }
    }

    /// Every page renders the scaffolding `scripts/check-docs.sh` gates on
    /// (frontmatter, H1, TL;DR) plus the generated-file marker, so the
    /// generated tree needs no docs-check carve-out.
    #[test]
    fn every_page_renders_compliant_scaffolding() {
        for page in pages() {
            let rendered = page.render();
            let file = page.file;
            assert!(
                rendered.starts_with("---\naudience: "),
                "{file}: page must open with YAML frontmatter"
            );
            assert!(
                rendered.contains(&format!(
                    "last-reviewed: {}\n",
                    super::GENERATED_LAST_REVIEWED
                )),
                "{file}: frontmatter must carry the generator-owned date"
            );
            assert!(
                rendered.contains("\n**TL;DR.** "),
                "{file}: page must carry a TL;DR block"
            );
            assert!(
                rendered.contains("GENERATED FILE"),
                "{file}: page must carry the generated-file marker"
            );
            assert!(
                rendered.contains("just docs-gen"),
                "{file}: the marker must name the regeneration remedy"
            );
            assert!(
                rendered.ends_with('\n') && !rendered.ends_with("\n\n"),
                "{file}: page must end with exactly one trailing newline"
            );
        }
    }

    /// The index must route to every content page, so registering a new
    /// renderer cannot silently leave it unlisted.
    #[test]
    fn index_lists_every_content_page() {
        let pages = pages();
        let (index, content): (Vec<&Page>, Vec<&Page>) =
            pages.iter().partition(|page| page.file == "README.md");
        assert_eq!(index.len(), 1, "exactly one index page");
        for page in content {
            assert!(
                index[0].body.contains(&format!("[{0}]({0})", page.file)),
                "index README.md does not link {}",
                page.file
            );
        }
    }
}
