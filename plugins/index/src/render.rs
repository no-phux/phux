//! JSON and Markdown renderings. Field order and sort order are part of the
//! golden output: the same inputs always produce the same bytes.

use crate::{Error, Index, NOTICE};

/// Pretty JSON with a trailing newline.
///
/// # Errors
///
/// Returns an error only if the document cannot be serialized.
pub fn render_json(index: &Index) -> Result<String, Error> {
    let mut text = serde_json::to_string_pretty(index)
        .map_err(|err| Error::new(format!("could not serialize the index: {err}")))?;
    text.push('\n');
    Ok(text)
}

/// Human-readable page. The first paragraph is [`NOTICE`].
#[must_use]
pub fn render_markdown(index: &Index) -> String {
    let mut out = String::new();
    out.push_str("# phux plugin index\n\n");
    out.push_str(NOTICE);
    out.push_str("\n\nGenerated ");
    out.push_str(&index.generated_at);
    out.push_str(".\n\n");
    out.push_str("| Name | Description | Kinds | Repository | Subdirectory | Stars | Updated | License | Install |\n");
    out.push_str("| --- | --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for plugin in &index.plugins {
        let kinds = if plugin.kinds.is_empty() {
            "-".to_owned()
        } else {
            plugin.kinds.join(", ")
        };
        let subdir = if plugin.subdir.is_empty() {
            "-"
        } else {
            plugin.subdir.as_str()
        };
        let stars = plugin
            .stars
            .map(|count| count.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let updated = plugin.pushed_at.as_deref().unwrap_or("-");
        let license = plugin.license.as_deref().unwrap_or("-");
        out.push_str("| ");
        out.push_str(&cell(&plugin.name));
        out.push_str(" | ");
        out.push_str(&cell(&plugin.description));
        out.push_str(" | ");
        out.push_str(&cell(&kinds));
        out.push_str(" | ");
        out.push_str(&cell(&plugin.repo));
        out.push_str(" | ");
        out.push_str(&cell(subdir));
        out.push_str(" | ");
        out.push_str(&cell(&stars));
        out.push_str(" | ");
        out.push_str(&cell(updated));
        out.push_str(" | ");
        out.push_str(&cell(license));
        out.push_str(" | `");
        out.push_str(&plugin.install);
        out.push_str("` |\n");
    }
    out.push_str("\n## Skipped\n\n");
    if index.skipped.is_empty() {
        out.push_str("None.\n");
    } else {
        for skip in &index.skipped {
            out.push_str("- `");
            out.push_str(&escape_markdown(&skip.repo));
            out.push('`');
            if !skip.subdir.is_empty() {
                out.push_str(" (`");
                out.push_str(&escape_markdown(&skip.subdir));
                out.push_str("`)");
            }
            out.push_str(" — ");
            out.push_str(&one_cell(&skip.reason));
            out.push('\n');
        }
    }
    out
}

fn cell(text: &str) -> String {
    escape_markdown(text)
}

fn one_cell(text: &str) -> String {
    escape_markdown(text)
}

/// Escape characters that would start a link, an image, an autolink, or a new table cell.
fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            // `@` breaks GitHub's bare e-mail autolink.
            '\\' | '`' | '[' | ']' | '(' | ')' | '!' | '*' | '_' | '|' | '<' | '>' | '@' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' | '\r' => out.push(' '),
            _ => out.push(ch),
        }
    }
    // GitHub autolinks a bare URL even inside a table cell, whatever the
    // scheme's case (`HTTPS://`, `ftp://`), and a bare `www.` host.
    defuse_autolinks(&out)
}

/// Break every `scheme://` and `www.` prefix, case-insensitively, so GitHub
/// does not autolink it: `https[:]//`, `www[.]`.
fn defuse_autolinks(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len() + 8);
    let mut rest = 0;
    let mut i = 0;
    while i < text.len() {
        if lower[i..].starts_with("://") {
            out.push_str(&text[rest..i]);
            out.push_str("[:]//");
            i += 3;
            rest = i;
        } else if lower[i..].starts_with("www.") {
            out.push_str(&text[rest..i + 3]);
            out.push_str("[.]");
            i += 4;
            rest = i;
        } else {
            // Step one character, not one byte: slicing inside a multi-byte
            // character panics.
            i += text[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out.push_str(&text[rest..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Index, NOTICE, Plugin, SCHEMA_VERSION, Skipped};

    #[test]
    fn names_cannot_inject_a_markdown_link_or_image() {
        let index = Index {
            schema_version: SCHEMA_VERSION,
            generated_at: "2026-01-15T00:00:00Z".to_owned(),
            notice: NOTICE.to_owned(),
            plugins: vec![Plugin {
                repo: "acme/widgets".to_owned(),
                subdir: String::new(),
                id: "acme.widgets".to_owned(),
                name: "![x](https://evil.example/a.png)".to_owned(),
                description: "[click](https://evil.example)".to_owned(),
                kinds: vec!["actions".to_owned()],
                min_phux_version: "0.0.2".to_owned(),
                stars: Some(1),
                pushed_at: None,
                license: None,
                install: "phux plugin install https://github.com/acme/widgets".to_owned(),
            }],
            skipped: Vec::new(),
        };
        let markdown = render_markdown(&index);
        assert!(!markdown.contains("]("), "{markdown}");
        assert!(!markdown.contains("!["), "{markdown}");
        assert!(markdown.contains("\\[click\\]"), "{markdown}");
        assert!(markdown.contains("\\!\\[x\\]"), "{markdown}");
    }

    /// Spam links, not script injection: GitHub autolinks these shapes even
    /// when the scheme is upper-case or absent.
    #[test]
    fn autolinks_are_defused_whatever_their_case_or_scheme() {
        for (raw, defused) in [
            ("HTTPS://evil.example", "HTTPS[:]//evil.example"),
            ("ftp://evil.example", "ftp[:]//evil.example"),
            ("see www.evil.example", "see www[.]evil.example"),
            ("WWW.evil.example", "WWW[.]evil.example"),
            ("mail me@evil.example", "mail me\\@evil.example"),
            ("https://ok.example", "https[:]//ok.example"),
            ("héllo wörld — www.ok", "héllo wörld — www[.]ok"),
        ] {
            assert_eq!(escape_markdown(raw), defused);
        }
    }

    /// The subdir alphabet (`validate.rs`) and this escape cover for each
    /// other; a hostile subdir must still render inert if either loosens.
    #[test]
    fn a_hostile_skipped_subdir_renders_inert() {
        let index = Index {
            schema_version: SCHEMA_VERSION,
            generated_at: "2026-01-15T00:00:00Z".to_owned(),
            notice: NOTICE.to_owned(),
            plugins: Vec::new(),
            skipped: vec![Skipped {
                repo: "acme/widgets".to_owned(),
                subdir: "plugins/a`b\n- [x](https://evil.example)".to_owned(),
                reason: "invalid plugin directory".to_owned(),
            }],
        };
        let markdown = render_markdown(&index);
        let line = markdown
            .lines()
            .find(|line| line.starts_with("- `acme/widgets`"))
            .expect("the skip renders as one list item");
        assert!(
            line.contains("a\\`b"),
            "the backtick must not close the code span: {line}"
        );
        assert!(!line.contains("]("), "{line}");
        assert!(
            !markdown.contains("\n- [x]"),
            "a newline must not start a new item:\n{markdown}"
        );
    }
}
