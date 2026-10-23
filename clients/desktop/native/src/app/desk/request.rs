//! Window launch and update-restoration requests.

use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WindowRole {
    #[default]
    Main,
    Fresh,
    Quick,
}

#[derive(Clone, Debug, Default)]
pub struct OpenRequest {
    /// An update restart carries the exact window, including secondary windows.
    pub resume_layout: Option<persist::SavedLayout>,
    pub authority: Option<phux_protocol::ids::AuthorityId>,
    pub role: WindowRole,
    pub session: String,
    pub home_key: Option<SessionKey>,
    /// Spawn a new terminal when nothing is restored, instead of showing the home pane.
    pub fresh: bool,
    pub initial_terminal: Option<ResourceId>,
    pub layout_path: PathBuf,
    pub legacy_layout: Option<PathBuf>,
    /// Default stores adopt the session key after the first topology; explicit
    /// layout paths and fresh windows keep their caller-selected store.
    pub stable_layout: bool,
    pub startup_error: Option<String>,
    /// The main window's display, which a quick window borrows.
    pub main_display: Option<DisplayPrefs>,
    /// A host from the remote registry. Absent is the local server.
    pub remote: Option<String>,
}
