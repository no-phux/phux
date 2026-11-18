//! Host path search and request correlation in the palette.

use phux_client_runtime::Client;
use phux_client_runtime::control::{PathMatchKind, PathSearchStatus};
use phux_protocol::ids::ResourceId;

use super::{Desk, Mode, NoticeKind};

/// A path search on the focused pane's host.
#[derive(Debug, Clone, PartialEq)]
pub struct PathSearch {
    /// The pane a picked path goes to.
    pub terminal: ResourceId,
    /// Its folder: the search's root, and what paths read relative to.
    pub origin: String,
    /// The request whose answer counts; later ones replace it.
    pub(super) pending: u32,
    /// Absolute paths in the server's order, and whether each is a folder.
    pub rows: Vec<(String, bool)>,
    /// Why there is nothing to show, or what the answer left out.
    pub note: Option<String>,
}

impl PathSearch {
    pub const fn searching(&self) -> bool {
        self.pending != 0
    }
}

impl Desk {
    /// Paths under the focused pane's folder, searched on its host.
    pub(in crate::desk) fn open_paths(&mut self) {
        let Some(terminal) = self.connection.shown.as_ref().map(|s| s.resource.clone()) else {
            return;
        };
        let Some(origin) = self.focused_cwd() else {
            return self.say(
                NoticeKind::Error,
                "This terminal hasn't said which folder it is in.",
            );
        };
        self.open_palette(Mode::Paths);
        self.connection.paths = Some(PathSearch {
            terminal,
            origin,
            pending: 0,
            rows: Vec::new(),
            note: None,
        });
        self.search_paths();
    }

    /// Ask the host for the paths the query names; an empty query lists the
    /// folder itself.
    pub(super) fn search_paths(&mut self) {
        let (Some(client), Some(search)) =
            (&self.connection.client, self.connection.paths.as_mut())
        else {
            return;
        };
        let query = self.palette_query.text.trim().to_owned();
        let host = search.terminal.host().cloned();
        let recursive = !query.is_empty();
        search.pending = client.path_query(search.origin.clone(), query, recursive, host);
        search.rows.clear();
        search.note =
            (search.pending == 0).then(|| "Path search is unavailable on this server.".to_owned());
    }

    /// Answers to the path search. True when the list changed.
    pub(in crate::desk) fn take_paths(&mut self, client: &Client) -> bool {
        let answers = client.take_path_answers();
        let Some(search) = self.connection.paths.as_mut() else {
            return false;
        };
        let Some(answer) = answers
            .into_iter()
            .rfind(|a| a.request_id == search.pending)
        else {
            return false;
        };
        search.pending = 0;
        search.rows = answer
            .rows
            .iter()
            .map(|row| (row.path.clone(), row.kind == PathMatchKind::Directory))
            .collect();
        search.note = match (answer.failure, answer.status) {
            (Some(_), _) if !answer.message.is_empty() => Some(answer.message),
            (Some(_), _) => Some("The host couldn't search here.".to_owned()),
            (None, Some(PathSearchStatus::Truncated)) => {
                Some("More match; keep typing to narrow them.".to_owned())
            }
            (None, Some(PathSearchStatus::Warming)) => {
                Some("The host is still indexing; try again shortly.".to_owned())
            }
            _ => None,
        };
        self.palette_picked = 0;
        true
    }
}
