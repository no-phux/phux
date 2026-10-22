//! Generated reference documentation behind the hidden `phux
//! gen-reference-docs` subcommand. [`pages`] is the single registry: the
//! generator writes it, the freshness test compares it, and the index lists
//! exactly these pages. Rendering is deterministic (no timestamps,
//! environment, or terminal width).

pub(crate) mod actions;
pub(crate) mod cli;
pub(crate) mod config;
pub(crate) mod deprecations;
pub(crate) mod environment;
pub(crate) mod exit_codes;
pub(crate) mod files;
pub(crate) mod hooks;
pub(crate) mod keys;
pub(crate) mod kinds;
pub(crate) mod parity;
pub(crate) mod widgets;

/// The warning embedded in every generated page, after the lede.
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
    /// One-sentence description under the H1.
    lede: &'static str,
    /// Markdown body following the generated-file marker.
    body: String,
}

impl Page {
    /// The complete file contents: H1, lede, generated-file marker, body --
    /// exactly what the generator writes and the freshness test expects on
    /// disk.
    pub(crate) fn render(&self) -> String {
        let Self {
            title, lede, body, ..
        } = self;
        format!(
            "# {title}\n\
             \n\
             {lede}\n\
             \n\
             {GENERATED_MARKER}\n\
             \n\
             {}\n",
            body.trim_end()
        )
    }
}

/// The full page registry: the index first, then every content page. Add a
/// new generated reference by pushing its `Page` here.
pub(crate) fn pages() -> Vec<Page> {
    let content = vec![
        cli::page(),
        config::page(),
        keys::page(),
        actions::page(),
        widgets::page(),
        hooks::page(),
        exit_codes::page(),
        environment::page(),
        files::page(),
        deprecations::page(),
        kinds::page(),
        parity::page(),
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
        "| Page | Contents |\n\
         |---|---|\n",
    );
    for page in content {
        let Page {
            file,
            title,
            summary,
            ..
        } = page;
        let _ = writeln!(body, "| [{title}]({file}) | {summary} |");
    }
    body.push_str(
        "\nWorking on phux? Run `just docs-gen` after changing a command, \
         setting, or anything else listed here; a unit test fails until the \
         pages match the binary.\n",
    );
    Page {
        file: "README.md",
        title: "All reference pages",
        summary: "Index of the generated reference pages.",
        lede: "Every page here is generated from the `phux` binary, so it \
               matches the code the rest of the docs describe.",
        body,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{Page, pages};

    /// `docs/reference/` in this checkout, resolved from the crate root so
    /// the test works from any test-runner working directory.
    fn reference_dir() -> PathBuf {
        std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR")
                .expect("the test runner sets CARGO_MANIFEST_DIR"),
        )
        .join("../../docs/reference")
    }

    /// The freshness gate: every registered page matches its on-disk bytes and
    /// every on-disk file is registered. Remedy: `just docs-gen`.
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

    /// Every page opens with its H1 and carries the generated-file marker.
    #[test]
    fn every_page_renders_its_scaffolding() {
        for page in pages() {
            let rendered = page.render();
            let file = page.file;
            assert!(
                rendered.starts_with("# "),
                "{file}: page must open with its H1"
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
                index[0].body.contains(&format!("]({})", page.file)),
                "index README.md does not link {}",
                page.file
            );
        }
    }
}
