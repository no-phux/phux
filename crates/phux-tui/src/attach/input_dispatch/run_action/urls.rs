//! `url-picker`: every link in the focused pane, newest first, to copy.

use phux_config::keybind::ResolvedAction;

use crate::attach::copy::extract_selection_text;
use crate::attach::input_dispatch::args::str_arg;
use crate::attach::pane_state::published_terminal;
use crate::render::overlay::{CopyRequest, SelectItem, SelectList, SelectionGrab};

use super::ActionCall;

/// The most links one picker lists.
const MAX_LINKS: usize = 200;

/// The `http(s)://` links in `text`, newest (last) first, each once.
fn links(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("http") {
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c.is_control() || "\"'<>`|".contains(c))
            .unwrap_or(tail.len());
        // Prose punctuation after a link is not part of it.
        let link = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}']);
        let scheme = link.starts_with("http://") || link.starts_with("https://");
        if scheme && link.contains('.') && !found.iter().any(|seen| seen == link) {
            found.push(link.to_owned());
        }
        rest = &tail[end.max(4)..];
    }
    found.reverse();
    found.truncate(MAX_LINKS);
    found
}

/// List the focused pane's links, its history included; choosing one copies
/// it to the clipboard.
// ponytail: copies rather than opens, because over ssh "open" would run on
// the far machine; add an opener for a local attach if copy-paste is a drag.
pub(in crate::attach) fn url_picker(
    ActionCall {
        ctx,
        focused,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    let all = CopyRequest {
        start_row: 0,
        start_col: 0,
        end_row: 0,
        end_col: 0,
        mouse_anchor_screen: None,
        rectangle: false,
        cursor_row: 0,
        cursor_col: 0,
        grab: SelectionGrab::All,
    };
    let text = focused
        .and_then(|id| published_terminal(ctx.engine_kernel, id))
        .and_then(|terminal| extract_selection_text(terminal, all))
        .unwrap_or_default();
    let found = links(&text);
    if found.is_empty() {
        *ctx.rename_notice = Some("no links in this pane".to_owned());
        effects.bell = true;
        return;
    }
    let items = found
        .into_iter()
        .map(|link| {
            let mut action = ResolvedAction {
                action: "copy-text".to_owned(),
                args: std::collections::BTreeMap::default(),
            };
            action.args.insert("text".to_owned(), link.clone().into());
            SelectItem::new(link, action)
        })
        .collect();
    ctx.overlays
        .push(Box::new(SelectList::new("Copy a link", items, ctx.theme)));
    effects.layout_mutated = true;
}

/// Put `text` on the clipboard: what a picker row does when chosen.
pub(in crate::attach) fn copy_text(
    ActionCall {
        resolved, effects, ..
    }: ActionCall<'_, '_>,
) {
    effects.clipboard = str_arg(resolved, "text");
}

/// `copy-mode-key` outside copy mode does nothing: it is read as an entry
/// of the `copy-mode` key mode, where the overlay applies it.
pub(in crate::attach) const fn copy_mode_key(ActionCall { effects, .. }: ActionCall<'_, '_>) {
    effects.bell = true;
}

#[cfg(test)]
mod tests {
    use super::links;

    #[test]
    fn links_are_found_newest_first_without_trailing_punctuation() {
        let text = "see https://a.example/x?y=1, then (http://b.example/p).\n\
                    again https://a.example/x?y=1 and \"https://c.example\" httpd http://nodot";
        assert_eq!(
            links(text),
            [
                "https://c.example",
                "http://b.example/p",
                "https://a.example/x?y=1"
            ]
        );
        assert!(links("nothing here").is_empty());
    }
}
