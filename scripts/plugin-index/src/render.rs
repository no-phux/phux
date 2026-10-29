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
            out.push_str(&skip.repo);
            out.push('`');
            if !skip.subdir.is_empty() {
                out.push_str(" (`");
                out.push_str(&skip.subdir);
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
    one_cell(text).replace('|', "\\|")
}

fn one_cell(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
