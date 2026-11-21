//! Where each archived session works, gathered from the server.

use std::collections::HashMap;
use std::path::Path;

/// Where each session (by name) works: its project, host, and directory, and
/// the project link it carries.
pub(super) struct Places {
    pub(super) projects: HashMap<String, String>,
    pub(super) hosts: HashMap<String, String>,
    pub(super) directories: HashMap<String, String>,
    pub(super) links: HashMap<String, String>,
}

/// Each session's place. Typed attributes are authoritative and per-session;
/// singleton metadata values remain a fallback for older sessions and
/// archives.
pub(super) async fn session_places(
    socket_path: &Path,
    sessions: Vec<phux_protocol::wire::frame::SessionRecord>,
) -> Places {
    let mut places = Places {
        projects: phux_client::workspace_place::fetch_project_by_session(socket_path).await,
        hosts: phux_client::workspace_place::fetch_host_by_session(socket_path).await,
        directories: phux_client::workspace_place::fetch_directory_by_session(socket_path).await,
        links: HashMap::new(),
    };
    for session in sessions {
        for (key, value) in session.attrs {
            let map = match key.as_str() {
                "project" => &mut places.projects,
                phux_core::project_identity::attr::LINK => &mut places.links,
                "directory" => &mut places.directories,
                "host" => &mut places.hosts,
                _ => continue,
            };
            map.insert(session.name.clone(), value);
        }
    }
    places
}
