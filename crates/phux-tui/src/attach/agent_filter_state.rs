//! The sidebar Agents panel's filter, kept across attaches and session
//! switches: per-user client state under the profile's state dir, never on
//! the server. Best effort, like `project_collapse`: an unreadable file
//! reads as the default (every agent) and a failed write is logged.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::render::chrome::sidebar::{AgentFilter, AgentScope, HostFilter};

const STATE_FILE: &str = "agents-filter.json";
const STATE_VERSION: u8 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    version: u8,
    /// An `agents-filter { scope }` value.
    #[serde(default)]
    scope: String,
    /// An `agents-filter { host }` value.
    #[serde(default)]
    host: String,
}

fn state_path() -> PathBuf {
    phux_config::instance::state_dir().join(STATE_FILE)
}

/// The filter this client saved last.
pub(super) fn load() -> AgentFilter {
    load_from(&state_path())
}

/// Save `filter` for the next attach.
pub(super) fn save(filter: &AgentFilter) {
    if let Err(err) = save_to(&state_path(), filter) {
        tracing::debug!(error = %err, "could not save the agents filter");
    }
}

fn load_from(path: &Path) -> AgentFilter {
    let Some(state) = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<State>(&bytes).ok())
    else {
        return AgentFilter::default();
    };
    AgentFilter {
        scope: AgentScope::parse(&state.scope).unwrap_or_default(),
        host: HostFilter::parse(&state.host),
    }
}

/// Write through a sibling temp file and rename, so a reader never sees a
/// torn file.
fn save_to(path: &Path, filter: &AgentFilter) -> std::io::Result<()> {
    let state = State {
        version: STATE_VERSION,
        scope: filter.scope.name().to_owned(),
        host: filter.host.name().to_owned(),
    };
    let bytes = serde_json::to_vec(&state).map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn the_filter_round_trips_and_bad_files_read_as_the_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join(STATE_FILE);
        assert_eq!(load_from(&path), AgentFilter::default());
        let filter = AgentFilter {
            scope: AgentScope::Attention,
            host: HostFilter::Host("edge".to_owned()),
        };
        save_to(&path, &filter).expect("save");
        assert_eq!(load_from(&path), filter);
        std::fs::write(&path, b"{not json").expect("corrupt");
        assert_eq!(load_from(&path), AgentFilter::default());
    }
}

#[cfg(test)]
mod frozen {
    #[test]
    fn agents_filter() {
        phux_frozen::json::<super::State>("agents-filter").check();
    }
}
