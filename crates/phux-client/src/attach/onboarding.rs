//! Moment-based first-use guidance.
//!
//! The journey is profile-scoped under phux's state directory and deliberately
//! best-effort: state failures suppress guidance rather than preventing attach.
//! The command palette can always reopen the introduction.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use phux_config::KeybindingsCfg;
use phux_config::keybind::ResolvedAction;
use serde::{Deserialize, Serialize};

const STATE_VERSION: u8 = 1;
const STATE_FILE: &str = "onboarding.json";

pub(super) const ONBOARDING_TITLE: &str = "Your session is live";
pub(super) const RETURN_NOTICE: &str = "Welcome back - this is the session you left running";
pub(super) const DETACH_NOTICE: &str =
    "phux: session still running; run `phux` when you want to come back";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttachMoment {
    Intro,
    Return,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Stage {
    IntroShown,
    DetachedOnce,
    Complete,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u8,
    stage: Stage,
}

pub(super) fn state_path() -> PathBuf {
    state_path_in(&phux_config::instance::state_dir())
}

fn state_path_in(state_dir: &Path) -> PathBuf {
    state_dir.join(STATE_FILE)
}

/// Advance the attach side of the journey. All I/O is best-effort.
pub(super) fn begin_attach(path: &Path) -> AttachMoment {
    match read_stage(path) {
        ReadState::Missing => {
            let _ = write_stage(path, Stage::IntroShown);
            AttachMoment::Intro
        }
        ReadState::Known(Stage::DetachedOnce) => {
            let _ = write_stage(path, Stage::Complete);
            AttachMoment::Return
        }
        ReadState::Known(Stage::IntroShown | Stage::Complete) | ReadState::Quiet => {
            AttachMoment::None
        }
    }
}

/// Advance the first intentional-detach moment and return its cooked-terminal
/// reassurance. Other exits and state failures remain quiet.
pub(super) fn after_detach(path: &Path) -> Option<&'static str> {
    if read_stage(path) != ReadState::Known(Stage::IntroShown) {
        return None;
    }
    let _ = write_stage(path, Stage::DetachedOnce);
    Some(DETACH_NOTICE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadState {
    Missing,
    Known(Stage),
    Quiet,
}

fn read_stage(path: &Path) -> ReadState {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == ErrorKind::NotFound => return ReadState::Missing,
        Err(_) => return ReadState::Quiet,
    };
    let Ok(state) = serde_json::from_slice::<State>(&bytes) else {
        return ReadState::Quiet;
    };
    if state.version != STATE_VERSION {
        return ReadState::Quiet;
    }
    ReadState::Known(state.stage)
}

fn write_stage(path: &Path, stage: Stage) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "state path has no parent",
        ));
    };
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec(&State {
        version: STATE_VERSION,
        stage,
    })
    .map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            Err(err)
        }
    }
}

/// Compact guidance using the bindings active for this attach invocation.
pub(super) fn hint_lines(keybindings: Option<&KeybindingsCfg>) -> Vec<String> {
    let detach = binding_label(keybindings, "detach", "Detach action");
    let palette = binding_label(keybindings, "command-palette", "Command palette");
    vec![
        "Keep working normally. phux keeps this session alive when you leave.".to_owned(),
        String::new(),
        format!("  {detach:<18} leave this view"),
        "  phux               come back from any shell".to_owned(),
        format!("  {palette:<18} browse commands"),
        String::new(),
        "Getting started stays available in the command palette.".to_owned(),
    ]
}

fn binding_label(keybindings: Option<&KeybindingsCfg>, action: &str, fallback: &str) -> String {
    let resolved = ResolvedAction {
        action: action.to_owned(),
        args: BTreeMap::new(),
    };
    super::action_registry::bound_chord_for(keybindings, &resolved)
        .unwrap_or_else(|| fallback.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_keybindings() -> KeybindingsCfg {
        phux_config::parse_str(phux_config::DEFAULT_CONFIG_TOML, Path::new("default.toml"))
            .expect("defaults parse")
            .keybindings
    }

    #[test]
    fn journey_advances_by_moment_and_then_stays_quiet() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = state_path_in(tmp.path());

        assert_eq!(begin_attach(&path), AttachMoment::Intro);
        assert_eq!(begin_attach(&path), AttachMoment::None);
        assert_eq!(after_detach(&path), Some(DETACH_NOTICE));
        assert_eq!(after_detach(&path), None);
        assert_eq!(begin_attach(&path), AttachMoment::Return);
        assert_eq!(begin_attach(&path), AttachMoment::None);
    }

    #[test]
    fn separate_profile_directories_have_independent_journeys() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let installed = state_path_in(&tmp.path().join("phux"));
        let dev = state_path_in(&tmp.path().join("phux-dev"));

        assert_eq!(begin_attach(&installed), AttachMoment::Intro);
        assert_eq!(after_detach(&installed), Some(DETACH_NOTICE));
        assert_eq!(begin_attach(&dev), AttachMoment::Intro);
        assert_eq!(begin_attach(&installed), AttachMoment::Return);
        assert_eq!(begin_attach(&dev), AttachMoment::None);
    }

    #[test]
    fn corrupt_and_future_state_fail_quiet() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = state_path_in(tmp.path());
        std::fs::write(&path, b"not json").expect("write corrupt state");
        assert_eq!(begin_attach(&path), AttachMoment::None);
        assert_eq!(after_detach(&path), None);

        std::fs::write(&path, br#"{"version":2,"stage":"intro-shown"}"#)
            .expect("write future state");
        assert_eq!(begin_attach(&path), AttachMoment::None);
    }

    #[test]
    fn guidance_tracks_rebound_actions() {
        let mut keys = default_keybindings();
        keys.prefix = "C-b".to_owned();
        keys.prefix_table.retain(|_, action| {
            !matches!(
                action,
                phux_config::Action::Bare(name)
                    if matches!(name.as_str(), "detach" | "command-palette")
            )
        });
        keys.prefix_table.insert(
            "x".to_owned(),
            phux_config::Action::Bare("detach".to_owned()),
        );
        keys.prefix_table.insert(
            "p".to_owned(),
            phux_config::Action::Bare("command-palette".to_owned()),
        );
        let body = hint_lines(Some(&keys)).join("\n");
        assert!(body.contains("C-b x"), "detach binding:\n{body}");
        assert!(body.contains("C-b p"), "palette binding:\n{body}");
        assert!(!body.contains("C-a d"), "must not advertise stale defaults");
    }
}
