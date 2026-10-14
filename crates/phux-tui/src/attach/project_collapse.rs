//! Which project groups this client keeps collapsed in the sidebar
//! (ADR-0167): per-user client state under the profile's state dir, never
//! on the server. Best effort: an unreadable file reads as nothing
//! collapsed and a failed write is logged.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const STATE_FILE: &str = "collapsed-projects.json";
const STATE_VERSION: u8 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    version: u8,
    #[serde(default)]
    collapsed: BTreeSet<String>,
}

fn state_path() -> PathBuf {
    phux_config::instance::state_dir().join(STATE_FILE)
}

/// The collapsed group keys this client saved last.
pub(super) fn load() -> BTreeSet<String> {
    load_from(&state_path())
}

/// Save `collapsed` for the next attach.
pub(super) fn save(collapsed: &BTreeSet<String>) {
    if let Err(err) = save_to(&state_path(), collapsed) {
        tracing::debug!(error = %err, "could not save collapsed project groups");
    }
}

fn load_from(path: &Path) -> BTreeSet<String> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<State>(&bytes).ok())
        .map(|state| state.collapsed)
        .unwrap_or_default()
}

/// Write through a sibling temp file and rename, so a reader never sees a
/// torn file.
fn save_to(path: &Path, collapsed: &BTreeSet<String>) -> std::io::Result<()> {
    let state = State {
        version: STATE_VERSION,
        collapsed: collapsed.clone(),
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
    fn collapsed_groups_round_trip_and_bad_files_read_as_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join(STATE_FILE);
        assert!(load_from(&path).is_empty());
        let set = BTreeSet::from(["github.com/no-phux/phux".to_owned()]);
        save_to(&path, &set).expect("save");
        assert_eq!(load_from(&path), set);
        std::fs::write(&path, b"{not json").expect("corrupt");
        assert!(load_from(&path).is_empty());
    }
}
