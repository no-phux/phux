//! Rows for the `go-to-directory` picker.
//!
//! The picker is a [`SelectList`](crate::render::overlay::SelectList) over
//! one `DIRECTORY_LISTING` reply (`docs/spec/L3.md` §4). The listing comes
//! from the server this client is attached to, so over `--remote` it browses
//! the remote host: that is the whole of the host-aware property, and it
//! needs no client-side knowledge of which host it is talking to.
//!
//! Every row commits an ordinary action, so navigation reuses the dispatcher
//! rather than growing a bespoke overlay:
//!
//! - "open new window here" commits `new-window { cwd = <path> }`, a new
//!   window in the current session that leaves the existing layout intact;
//! - `..` and each child directory commit `go-to-directory { path }`, which
//!   re-lists and reopens the picker at that path;
//! - a refused listing keeps `..` and `~` so the user is never stranded on a
//!   path they cannot read.
//!
//! Typing filters the rows (the [`SelectList`] fuzzy filter); Escape
//! dismisses.
//!
//! [`SelectList`]: crate::render::overlay::SelectList

use std::collections::BTreeMap;
use std::path::Path;

use phux_config::keybind::ResolvedAction;
use phux_protocol::wire::frame::{
    DirectoryEntry, DirectoryErrorCode, DirectoryListing, DirectoryListingError,
    DirectoryListingResult,
};

use crate::render::overlay::SelectItem;

/// The picker's modal title: the directory it shows.
pub(super) fn picker_title(result: &DirectoryListingResult) -> String {
    let path = match result {
        Ok(listing) => &listing.path,
        Err(error) => &error.path,
    };
    format!("go to directory: {path}")
}

/// The picker's rows for one listing reply.
pub(super) fn picker_items(result: &DirectoryListingResult) -> Vec<SelectItem> {
    match result {
        Ok(listing) => listing_items(listing),
        Err(error) => refusal_items(error),
    }
}

/// Open-here first (the confirm row), then `..`, then the children.
fn listing_items(listing: &DirectoryListing) -> Vec<SelectItem> {
    let mut items = vec![open_here_item(&listing.path)];
    items.extend(listing.parent.as_deref().map(parent_item));
    items.extend(ordered_children(&listing.entries).map(|entry| child_item(&listing.path, entry)));
    if listing.truncated {
        items.push(SelectItem::header(format!(
            "(listing truncated at {} directories)",
            listing.entries.len()
        )));
    }
    items
}

/// The reason, then the ways out: the parent of the refused path and home.
fn refusal_items(error: &DirectoryListingError) -> Vec<SelectItem> {
    let mut items = vec![SelectItem::header(format!(
        "{}: {}",
        refusal_reason(error.code),
        error.message
    ))];
    let parent = Path::new(&error.path).parent().and_then(Path::to_str);
    items.extend(parent.map(parent_item));
    items.push(SelectItem::new("~", go_to("~")).secondary("home"));
    items
}

const fn refusal_reason(code: DirectoryErrorCode) -> &'static str {
    match code {
        DirectoryErrorCode::NotFound => "not found",
        DirectoryErrorCode::PermissionDenied => "permission denied",
        DirectoryErrorCode::NotADirectory => "not a directory",
        DirectoryErrorCode::Other => "cannot list",
    }
}

/// Visible directories before dot-directories, each group in server order
/// (sorted by name).
fn ordered_children(entries: &[DirectoryEntry]) -> impl Iterator<Item = &DirectoryEntry> {
    let (hidden, visible): (Vec<_>, Vec<_>) = entries
        .iter()
        .partition(|entry| entry.name.starts_with('.'));
    visible.into_iter().chain(hidden)
}

fn open_here_item(path: &str) -> SelectItem {
    SelectItem::new("open new window here", open_window_at(path)).secondary("new window")
}

fn parent_item(parent: &str) -> SelectItem {
    SelectItem::new("..", go_to(parent)).secondary("parent")
}

fn child_item(dir: &str, entry: &DirectoryEntry) -> SelectItem {
    let item = SelectItem::new(
        format!("{}/", entry.name),
        go_to(&child_path(dir, &entry.name)),
    );
    if entry.is_symlink {
        item.secondary("symlink")
    } else {
        item
    }
}

/// `dir` joined with one child `name`, without doubling the root's slash.
fn child_path(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn go_to(path: &str) -> ResolvedAction {
    action_with("go-to-directory", "path", path)
}

fn open_window_at(path: &str) -> ResolvedAction {
    action_with("new-window", "cwd", path)
}

fn action_with(action: &str, key: &str, value: &str) -> ResolvedAction {
    let mut args = BTreeMap::new();
    args.insert(key.to_owned(), toml::Value::String(value.to_owned()));
    ResolvedAction {
        action: action.to_owned(),
        args,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_symlink: bool) -> DirectoryEntry {
        DirectoryEntry {
            name: name.to_owned(),
            is_symlink,
        }
    }

    fn listing(path: &str, parent: Option<&str>, entries: Vec<DirectoryEntry>) -> DirectoryListing {
        DirectoryListing {
            path: path.to_owned(),
            parent: parent.map(ToOwned::to_owned),
            entries,
            truncated: false,
        }
    }

    fn committed(item: &SelectItem) -> (&str, Option<&str>) {
        let arg = item
            .action
            .args
            .values()
            .next()
            .and_then(toml::Value::as_str);
        (item.action.action.as_str(), arg)
    }

    #[test]
    fn listing_rows_confirm_first_then_parent_then_visible_before_hidden() {
        let result = Ok(listing(
            "/home/u",
            Some("/home"),
            vec![
                entry(".cache", false),
                entry("src", false),
                entry("www", true),
            ],
        ));

        let items = picker_items(&result);

        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            ["open new window here", "..", "src/", "www/", ".cache/"]
        );
        assert_eq!(committed(&items[0]), ("new-window", Some("/home/u")));
        assert_eq!(committed(&items[1]), ("go-to-directory", Some("/home")));
        assert_eq!(
            committed(&items[2]),
            ("go-to-directory", Some("/home/u/src"))
        );
        assert_eq!(items[3].secondary.as_deref(), Some("symlink"));
        assert_eq!(picker_title(&result), "go to directory: /home/u");
    }

    #[test]
    fn the_root_has_no_parent_row_and_children_join_without_a_double_slash() {
        let items = picker_items(&Ok(listing("/", None, vec![entry("usr", false)])));

        assert_eq!(items.len(), 2);
        assert_eq!(committed(&items[1]), ("go-to-directory", Some("/usr")));
    }

    #[test]
    fn a_truncated_listing_ends_with_a_non_selectable_note() {
        let mut truncated = listing("/big", Some("/"), vec![entry("a", false)]);
        truncated.truncated = true;

        let items = picker_items(&Ok(truncated));

        assert!(items.last().is_some_and(SelectItem::is_header));
    }

    #[test]
    fn a_refusal_names_the_reason_and_keeps_parent_and_home_reachable() {
        let result = Err(DirectoryListingError {
            path: "/root/secret".to_owned(),
            code: DirectoryErrorCode::PermissionDenied,
            message: "Permission denied (os error 13)".to_owned(),
        });

        let items = picker_items(&result);

        assert!(items[0].is_header());
        assert!(items[0].label.starts_with("permission denied"));
        assert_eq!(committed(&items[1]), ("go-to-directory", Some("/root")));
        assert_eq!(committed(&items[2]), ("go-to-directory", Some("~")));
        assert_eq!(picker_title(&result), "go to directory: /root/secret");
    }
}
