//! Rows for the `go-to-directory` picker.
//!
//! The picker is a [`SelectList`](crate::render::overlay::SelectList) over
//! one `DIRECTORY_LISTING` reply (`docs/spec/L3.md` §4). The listing comes
//! from the server this client is attached to, so over `--remote` it browses
//! the remote host with no client-side knowledge of which host that is.
//!
//! Attached to a federation hub, a satellite pane's directories live on the
//! satellite, not the hub. When the hub advertises `LIST_DIRECTORY_HOST` the
//! request names that satellite ([`ListingHost::Satellite`]) and the hub
//! relays it (§4.1); every row then carries `host`, so browsing stays on the
//! satellite and "open new window here" spawns there. A hub without the bit
//! would ignore the field and list itself, so the request stays on the hub
//! and the picker says whose directories it shows
//! ([`ListingHost::AttachedInsteadOf`]).
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
use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::frame::{
    DirectoryEntry, DirectoryErrorCode, DirectoryListing, DirectoryListingError,
    DirectoryListingResult,
};

use crate::render::overlay::SelectItem;

/// What `go-to-directory` can list on the attached server, from its
/// negotiated feature bits. Fixed for the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DirectorySupport {
    /// No `LIST_DIRECTORY`: the action bells and sends nothing.
    Unsupported,
    /// `LIST_DIRECTORY` alone: only the attached server's own host.
    ServingHostOnly,
    /// `LIST_DIRECTORY_HOST` too: a hub lists a satellite on request.
    HostAware,
}

impl DirectorySupport {
    /// Read the support level off a server's advertised features.
    pub(super) const fn from_features(features: ServerFeatureSet) -> Self {
        if !features.contains(ServerFeature::ListDirectory) {
            return Self::Unsupported;
        }
        if features.contains(ServerFeature::ListDirectoryHost) {
            Self::HostAware
        } else {
            Self::ServingHostOnly
        }
    }
}

/// Which host one listing reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ListingHost {
    /// The attached server's own host (no `host` on the wire).
    Attached,
    /// A satellite, relayed through the attached hub (`LIST_DIRECTORY.host`).
    Satellite(SatelliteHost),
    /// The attached server's own host, listed in place of this satellite
    /// because the hub predates `LIST_DIRECTORY.host`.
    AttachedInsteadOf(SatelliteHost),
}

impl ListingHost {
    /// The satellite the request names on the wire, if any.
    pub(super) const fn satellite(&self) -> Option<&SatelliteHost> {
        match self {
            Self::Satellite(host) => Some(host),
            Self::Attached | Self::AttachedInsteadOf(_) => None,
        }
    }
}

/// The listing the picker waits on: its request id and the host it reads.
/// Newest request wins; a reply with any other id is stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingDirectory {
    /// The `LIST_DIRECTORY` correlation id.
    pub(super) request_id: u32,
    /// The host the listing reads, for the title and the rows.
    pub(super) host: ListingHost,
}

/// The picker's modal title: the directory it shows, naming the host
/// whenever it is not simply the attached server's.
pub(super) fn picker_title(result: &DirectoryListingResult, host: &ListingHost) -> String {
    let path = match result {
        Ok(listing) => &listing.path,
        Err(error) => &error.path,
    };
    match host {
        ListingHost::Attached => format!("go to directory: {path}"),
        ListingHost::Satellite(satellite) => format!("go to directory on {satellite}: {path}"),
        ListingHost::AttachedInsteadOf(_) => format!("go to directory on this host: {path}"),
    }
}

/// The picker's rows for one listing reply.
pub(super) fn picker_items(result: &DirectoryListingResult, host: &ListingHost) -> Vec<SelectItem> {
    let rows_host = host.satellite();
    let mut items: Vec<SelectItem> = fallback_note(host).into_iter().collect();
    items.extend(match result {
        Ok(listing) => listing_items(listing, rows_host),
        Err(error) => refusal_items(error, rows_host),
    });
    items
}

/// A hub that predates `LIST_DIRECTORY.host` lists only itself. Say so above
/// the rows, so its paths are not taken for the satellite pane's.
fn fallback_note(host: &ListingHost) -> Option<SelectItem> {
    let ListingHost::AttachedInsteadOf(satellite) = host else {
        return None;
    };
    Some(SelectItem::header(format!(
        "this hub cannot list {satellite}; showing its own host"
    )))
}

/// Open-here first (the confirm row), then `..`, then the children.
fn listing_items(listing: &DirectoryListing, host: Option<&SatelliteHost>) -> Vec<SelectItem> {
    let mut items = vec![open_here_item(&listing.path, host)];
    items.extend(
        listing
            .parent
            .as_deref()
            .map(|parent| parent_item(parent, host)),
    );
    items.extend(
        ordered_children(&listing.entries).map(|entry| child_item(&listing.path, entry, host)),
    );
    if listing.truncated {
        items.push(SelectItem::header(format!(
            "(listing truncated at {} directories)",
            listing.entries.len()
        )));
    }
    items
}

/// The reason, then the ways out: the parent of the refused path and home,
/// both on the host that refused.
fn refusal_items(error: &DirectoryListingError, host: Option<&SatelliteHost>) -> Vec<SelectItem> {
    let mut items = vec![SelectItem::header(format!(
        "{}: {}",
        refusal_reason(error.code),
        error.message
    ))];
    let parent = Path::new(&error.path).parent().and_then(Path::to_str);
    items.extend(parent.map(|parent| parent_item(parent, host)));
    items.push(SelectItem::new("~", go_to("~", host)).secondary("home"));
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

fn open_here_item(path: &str, host: Option<&SatelliteHost>) -> SelectItem {
    SelectItem::new("open new window here", open_window_at(path, host)).secondary("new window")
}

fn parent_item(parent: &str, host: Option<&SatelliteHost>) -> SelectItem {
    SelectItem::new("..", go_to(parent, host)).secondary("parent")
}

fn child_item(dir: &str, entry: &DirectoryEntry, host: Option<&SatelliteHost>) -> SelectItem {
    let item = SelectItem::new(
        format!("{}/", entry.name),
        go_to(&child_path(dir, &entry.name), host),
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

fn go_to(path: &str, host: Option<&SatelliteHost>) -> ResolvedAction {
    action_on("go-to-directory", "path", path, host)
}

fn open_window_at(path: &str, host: Option<&SatelliteHost>) -> ResolvedAction {
    action_on("new-window", "cwd", path, host)
}

/// `action { key = value }`, plus `host` when the rows list a satellite, so
/// every row keeps browsing, or opens its window, on the host it came from.
fn action_on(action: &str, key: &str, value: &str, host: Option<&SatelliteHost>) -> ResolvedAction {
    let mut args = BTreeMap::new();
    args.insert(key.to_owned(), toml::Value::String(value.to_owned()));
    if let Some(host) = host {
        args.insert(
            "host".to_owned(),
            toml::Value::String(host.as_str().to_owned()),
        );
    }
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

        let items = picker_items(&result, &ListingHost::Attached);

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
        assert_eq!(
            picker_title(&result, &ListingHost::Attached),
            "go to directory: /home/u"
        );
    }

    #[test]
    fn the_root_has_no_parent_row_and_children_join_without_a_double_slash() {
        let items = picker_items(
            &Ok(listing("/", None, vec![entry("usr", false)])),
            &ListingHost::Attached,
        );

        assert_eq!(items.len(), 2);
        assert_eq!(committed(&items[1]), ("go-to-directory", Some("/usr")));
    }

    #[test]
    fn a_truncated_listing_ends_with_a_non_selectable_note() {
        let mut truncated = listing("/big", Some("/"), vec![entry("a", false)]);
        truncated.truncated = true;

        let items = picker_items(&Ok(truncated), &ListingHost::Attached);

        assert!(items.last().is_some_and(SelectItem::is_header));
    }

    #[test]
    fn a_refusal_names_the_reason_and_keeps_parent_and_home_reachable() {
        let result = Err(DirectoryListingError {
            path: "/root/secret".to_owned(),
            code: DirectoryErrorCode::PermissionDenied,
            message: "Permission denied (os error 13)".to_owned(),
        });

        let items = picker_items(&result, &ListingHost::Attached);

        assert!(items[0].is_header());
        assert!(items[0].label.starts_with("permission denied"));
        assert_eq!(committed(&items[1]), ("go-to-directory", Some("/root")));
        assert_eq!(committed(&items[2]), ("go-to-directory", Some("~")));
        assert_eq!(
            picker_title(&result, &ListingHost::Attached),
            "go to directory: /root/secret"
        );
    }

    fn arg<'a>(item: &'a SelectItem, key: &str) -> Option<&'a str> {
        item.action.args.get(key).and_then(toml::Value::as_str)
    }

    fn edge() -> SatelliteHost {
        SatelliteHost::new("edge")
    }

    #[test]
    fn satellite_rows_keep_their_host_and_the_title_names_it() {
        let host = ListingHost::Satellite(edge());
        let result = Ok(listing("/home/e", Some("/home"), vec![entry("src", false)]));

        let items = picker_items(&result, &host);

        assert_eq!(
            picker_title(&result, &host),
            "go to directory on edge: /home/e"
        );
        assert_eq!(items[0].action.action, "new-window");
        assert_eq!(arg(&items[0], "cwd"), Some("/home/e"));
        assert_eq!(arg(&items[1], "path"), Some("/home"));
        assert_eq!(arg(&items[2], "path"), Some("/home/e/src"));
        for item in &items {
            assert_eq!(arg(item, "host"), Some("edge"), "{}", item.label);
        }
    }

    #[test]
    fn a_refused_satellite_listing_keeps_its_ways_out_on_that_host() {
        let host = ListingHost::Satellite(edge());
        let result = Err(DirectoryListingError {
            path: "/root".to_owned(),
            code: DirectoryErrorCode::Other,
            message: "satellite edge is unreachable: link is down".to_owned(),
        });

        let items = picker_items(&result, &host);

        assert!(items[0].is_header());
        assert!(items[0].label.contains("edge is unreachable"));
        assert!(
            items[1..]
                .iter()
                .all(|item| arg(item, "host") == Some("edge"))
        );
    }

    #[test]
    fn a_hub_without_host_listing_says_whose_directories_these_are() {
        let host = ListingHost::AttachedInsteadOf(edge());
        let result = Ok(listing("/home/hub", Some("/home"), Vec::new()));

        let items = picker_items(&result, &host);

        assert!(items[0].is_header());
        assert!(
            items[0].label.contains("cannot list edge"),
            "{}",
            items[0].label
        );
        assert_eq!(items[1].label, "open new window here");
        assert!(items.iter().all(|item| arg(item, "host").is_none()));
        assert_eq!(
            picker_title(&result, &host),
            "go to directory on this host: /home/hub"
        );
    }

    #[test]
    fn support_reads_the_listing_and_host_bits() {
        let listing = ServerFeatureSet::with(&[ServerFeature::ListDirectory]);
        let host_aware = ServerFeatureSet::with(&[
            ServerFeature::ListDirectory,
            ServerFeature::ListDirectoryHost,
        ]);
        let host_only = ServerFeatureSet::with(&[ServerFeature::ListDirectoryHost]);
        assert_eq!(
            DirectorySupport::from_features(ServerFeatureSet::new()),
            DirectorySupport::Unsupported
        );
        assert_eq!(
            DirectorySupport::from_features(listing),
            DirectorySupport::ServingHostOnly
        );
        assert_eq!(
            DirectorySupport::from_features(host_aware),
            DirectorySupport::HostAware
        );
        assert_eq!(
            DirectorySupport::from_features(host_only),
            DirectorySupport::Unsupported,
            "the host bit means nothing without the query itself"
        );
    }
}
