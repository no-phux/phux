//! What this user ran from the command palette lately.
//!
//! Recent commands lead an empty palette and outrank close matches, so the
//! palette learns its user. One action per line, newest first, in the state
//! directory.

use std::path::{Path, PathBuf};

use phux_config::keybind::ResolvedAction;

/// The most actions remembered.
const KEEP: usize = 20;
/// How many lead an empty palette.
pub const SHOWN: usize = 5;
/// The newest action's ranking boost; each older one earns [`BOOST_STEP`]
/// less, down to nothing.
const BOOST_TOP: i32 = 48;
const BOOST_STEP: i32 = 8;

/// The file the palette reads and writes.
#[must_use]
pub fn default_path() -> PathBuf {
    phux_config::instance::state_dir().join("palette-recent")
}

/// A stable one-line key for `action`: its name and args.
#[must_use]
pub fn key(action: &ResolvedAction) -> String {
    let mut key = action.action.clone();
    for (name, value) in &action.args {
        key.push(' ');
        key.push_str(name);
        key.push('=');
        key.push_str(&value.to_string());
    }
    key.replace('\n', " ")
}

/// The remembered keys at `path`, newest first; empty when unreadable.
#[must_use]
pub fn load(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().map(str::to_owned).take(KEEP).collect())
        .unwrap_or_default()
}

/// Remember `action` as the newest at `path`. Best effort: a palette that
/// cannot write still runs the command.
pub fn note(path: &Path, action: &ResolvedAction) {
    let newest = key(action);
    let mut keys = load(path);
    keys.retain(|key| *key != newest);
    keys.insert(0, newest);
    keys.truncate(KEEP);
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let partial = path.with_extension("partial");
    let mut text = keys.join("\n");
    text.push('\n');
    if std::fs::write(&partial, text).is_ok() {
        let _ = std::fs::rename(&partial, path);
    }
}

/// `action`'s ranking boost among `recent` keys (newest first).
#[must_use]
pub fn boost(recent: &[String], action: &ResolvedAction) -> i32 {
    let key = key(action);
    recent
        .iter()
        .position(|k| *k == key)
        .and_then(|rank| i32::try_from(rank).ok())
        .map_or(0, |rank| (BOOST_TOP - rank * BOOST_STEP).max(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(name: &str) -> ResolvedAction {
        crate::attach::resolved(name, std::collections::BTreeMap::new())
    }

    #[test]
    fn notes_newest_first_without_duplicates_and_boosts_by_recency() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state/palette-recent");
        assert!(load(&path).is_empty(), "nothing yet, nothing broken");
        note(&path, &act("split-pane"));
        note(&path, &act("new-window"));
        note(&path, &act("split-pane"));
        let recent = load(&path);
        assert_eq!(recent, ["split-pane", "new-window"]);
        assert!(boost(&recent, &act("split-pane")) > boost(&recent, &act("new-window")));
        assert_eq!(boost(&recent, &act("detach")), 0);
    }

    #[test]
    fn args_tell_variants_apart() {
        let mut below = act("split-pane");
        below
            .args
            .insert("direction".into(), toml::Value::String("down".into()));
        assert_ne!(key(&below), key(&act("split-pane")));
    }
}
