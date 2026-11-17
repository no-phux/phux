//! Per-user client state under the profile's state dir, never on the
//! server: the sidebar's Agents filter and its collapsed project groups
//! (ADR-0168). Best effort: an unreadable or newer file reads as the
//! default and a failed write is logged.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::render::chrome::sidebar::{AgentFilter, AgentScope, HostFilter};

const FILTER_FILE: &str = "agents-filter.json";
const COLLAPSED_FILE: &str = "collapsed-projects.json";
const STATE_VERSION: u8 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
struct FilterState {
    version: u8,
    /// An `agents-filter { scope }` value.
    #[serde(default)]
    scope: String,
    /// An `agents-filter { host }` value.
    #[serde(default)]
    host: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CollapsedState {
    version: u8,
    #[serde(default)]
    collapsed: BTreeSet<String>,
}

fn state_path(file: &str) -> PathBuf {
    phux_config::instance::state_dir().join(file)
}

/// The Agents filter this client saved last.
pub(super) fn filter() -> AgentFilter {
    agent_filter_from(&state_path(FILTER_FILE))
}

/// Save `filter` for the next attach.
pub(super) fn save_agent_filter(filter: &AgentFilter) {
    let state = FilterState {
        version: STATE_VERSION,
        scope: filter.scope.name().to_owned(),
        host: filter.host.name().to_owned(),
    };
    if let Err(err) = write_json(&state_path(FILTER_FILE), &state) {
        tracing::debug!(error = %err, "could not save the agents filter");
    }
}

/// The collapsed group keys this client saved last.
pub(super) fn collapsed() -> BTreeSet<String> {
    collapsed_from(&state_path(COLLAPSED_FILE))
}

/// Save `collapsed` for the next attach.
pub(super) fn save_collapsed_projects(collapsed: &BTreeSet<String>) {
    let state = CollapsedState {
        version: STATE_VERSION,
        collapsed: collapsed.clone(),
    };
    if let Err(err) = write_json(&state_path(COLLAPSED_FILE), &state) {
        tracing::debug!(error = %err, "could not save collapsed project groups");
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn agent_filter_from(path: &Path) -> AgentFilter {
    read_json::<FilterState>(path)
        .filter(|state| state.version <= STATE_VERSION)
        .map_or_else(AgentFilter::default, |state| AgentFilter {
            scope: AgentScope::parse(&state.scope).unwrap_or_default(),
            host: HostFilter::parse(&state.host),
        })
}

fn collapsed_from(path: &Path) -> BTreeSet<String> {
    read_json::<CollapsedState>(path)
        .filter(|state| state.version <= STATE_VERSION)
        .map(|state| state.collapsed)
        .unwrap_or_default()
}

/// Write through a sibling temp file and rename, so a reader never sees a
/// torn file.
fn write_json(path: &Path, state: &impl Serialize) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(state).map_err(std::io::Error::other)?;
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
    fn a_newer_format_reads_as_the_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let newer = STATE_VERSION + 1;
        let path = dir.path().join(FILTER_FILE);
        std::fs::write(
            &path,
            format!(r#"{{"version":{newer},"scope":"attention"}}"#),
        )
        .expect("write");
        assert_eq!(agent_filter_from(&path), AgentFilter::default());
        let path = dir.path().join(COLLAPSED_FILE);
        std::fs::write(&path, format!(r#"{{"version":{newer},"collapsed":["a"]}}"#))
            .expect("write");
        assert!(collapsed_from(&path).is_empty());
    }

    #[test]
    fn states_round_trip_and_bad_files_read_as_the_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join(FILTER_FILE);
        assert_eq!(agent_filter_from(&path), AgentFilter::default());
        let filter = AgentFilter {
            scope: AgentScope::Attention,
            host: HostFilter::Host("edge".to_owned()),
        };
        let state = FilterState {
            version: STATE_VERSION,
            scope: filter.scope.name().to_owned(),
            host: filter.host.name().to_owned(),
        };
        write_json(&path, &state).expect("save");
        assert_eq!(agent_filter_from(&path), filter);
        std::fs::write(&path, b"{not json").expect("corrupt");
        assert_eq!(agent_filter_from(&path), AgentFilter::default());

        let path = dir.path().join("nested").join(COLLAPSED_FILE);
        assert!(collapsed_from(&path).is_empty());
        let collapsed = BTreeSet::from(["github.com/no-phux/phux".to_owned()]);
        let state = CollapsedState {
            version: STATE_VERSION,
            collapsed: collapsed.clone(),
        };
        write_json(&path, &state).expect("save");
        assert_eq!(collapsed_from(&path), collapsed);
    }
}

#[cfg(test)]
mod frozen {
    #[test]
    fn agents_filter() {
        phux_frozen::json::<super::FilterState>("agents-filter").check();
    }

    #[test]
    fn collapsed_projects() {
        phux_frozen::json::<super::CollapsedState>("collapsed-projects").check();
    }
}
