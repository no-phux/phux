//! Moment-based first-use guidance.
//!
//! The journey is profile-scoped under phux's state directory and deliberately
//! best-effort: state failures suppress guidance rather than preventing attach.
//! The command palette can always reopen the introduction.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use phux_config::KeybindingsCfg;
use phux_config::keybind::ResolvedAction;
use serde::{Deserialize, Serialize};

const STATE_VERSION: u8 = 1;
const STATE_FILE: &str = "onboarding.json";
const LOCK_FILE: &str = "onboarding.lock";

pub(super) const ONBOARDING_TITLE: &str = "Your session is live";
pub(super) const RETURN_NOTICE: &str = "Welcome back - this is the session you left running";
pub(super) const DETACH_NOTICE: &str =
    "phux: session still running; rerun the same phux command to come back";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttachMoment {
    Intro,
    Return,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Stage {
    IntroPending,
    IntroShown,
    DetachedOnce,
    ReturnPending,
    Complete,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u8,
    stage: Stage,
}

/// An attach moment reserved under the profile lock.
///
/// Dropping an uncommitted claim leaves its pending stage retryable. The open
/// file is deliberately retained for the claim's entire lifetime: advisory
/// `flock` ownership is tied to that file description, not a lexical syscall.
#[derive(Debug)]
pub(super) struct AttachClaim {
    path: PathBuf,
    moment: AttachMoment,
    _lock: File,
}

impl AttachClaim {
    pub(super) const fn moment(&self) -> AttachMoment {
        self.moment
    }

    /// Persist successful delivery. Failure stays quiet and leaves the pending
    /// stage available for a later attach.
    pub(super) fn commit(self) -> bool {
        let stage = match self.moment {
            AttachMoment::Intro => Stage::IntroShown,
            AttachMoment::Return => Stage::Complete,
            AttachMoment::None => return false,
        };
        write_stage(&self.path, stage).is_ok()
    }
}

pub(super) fn state_path() -> PathBuf {
    state_path_in(&phux_config::instance::state_dir())
}

fn state_path_in(state_dir: &Path) -> PathBuf {
    state_dir.join(STATE_FILE)
}

/// Reserve the next attach moment. State and lock failures suppress guidance.
pub(super) fn begin_attach(path: &Path) -> Option<AttachClaim> {
    let lock = lock_state(path).ok()?;
    let (moment, pending) = match read_stage(path) {
        ReadState::Missing | ReadState::Known(Stage::IntroPending) => {
            (AttachMoment::Intro, Stage::IntroPending)
        }
        ReadState::Known(Stage::DetachedOnce | Stage::ReturnPending) => {
            (AttachMoment::Return, Stage::ReturnPending)
        }
        ReadState::Known(Stage::IntroShown | Stage::Complete) | ReadState::Quiet => return None,
    };
    write_stage(path, pending).ok()?;
    Some(AttachClaim {
        path: path.to_owned(),
        moment,
        _lock: lock,
    })
}

/// Advance the first intentional-detach moment and return its cooked-terminal
/// reassurance. Other exits and state failures remain quiet.
pub(super) fn after_detach(path: &Path) -> Option<&'static str> {
    let _lock = lock_state(path).ok()?;
    if read_stage(path) != ReadState::Known(Stage::IntroShown) {
        return None;
    }
    write_stage(path, Stage::DetachedOnce)
        .ok()
        .map(|()| DETACH_NOTICE)
}

fn lock_state(path: &Path) -> std::io::Result<File> {
    let Some(parent) = path.parent() else {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "state path has no parent",
        ));
    };
    std::fs::create_dir_all(parent)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(LOCK_FILE))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)?;
    Ok(file)
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
///
/// `sidebar_visible` is the attach's live sidebar flag (config seed at
/// first run, runtime toggle later). When the strip is already on, the
/// copy names the Agents list and glyphs and does not tell the user to
/// turn it on. When it is off, the copy names the live `toggle-sidebar`
/// chord.
pub(super) fn hint_lines(
    keybindings: Option<&KeybindingsCfg>,
    sidebar_visible: bool,
) -> Vec<String> {
    let detach = binding_label(keybindings, "detach", "Detach action");
    let palette = binding_label(keybindings, "command-palette", "Command palette");
    let sessions = binding_label(keybindings, "session-picker", "Sessions & hosts");
    let settings = binding_label(keybindings, "settings", "Settings");
    let copy = binding_label(keybindings, "copy-mode", "Copy mode");
    let mut lines = vec![
        "Keep working normally. phux keeps this session alive when you leave.".to_owned(),
        String::new(),
    ];
    lines.extend(inbox_lines(keybindings, sidebar_visible));
    lines.extend([
        format!("  {detach:<18} leave this view"),
        "  phux               come back from any shell".to_owned(),
        format!("  {sessions:<18} sessions across hosts"),
        format!("  {palette:<18} browse every command"),
        format!("  {settings:<18} edit configuration"),
        format!("  {copy:<18} select scrollback; Shift-drag uses host selection"),
        String::new(),
        destinations_line(sidebar_visible),
    ]);
    lines
}

fn inbox_lines(keybindings: Option<&KeybindingsCfg>, sidebar_visible: bool) -> Vec<String> {
    let blocked = crate::render::chrome::AGENT_BLOCKED_GLYPH;
    let working = crate::render::chrome::AGENT_WORKING_GLYPH;
    if sidebar_visible {
        vec![
            format!(
                "The Agents list is the fleet inbox. {blocked} means blocked on you; {working} means still working."
            ),
            String::new(),
        ]
    } else {
        let sidebar = binding_label(keybindings, "toggle-sidebar", "Toggle sidebar");
        vec![
            format!("  {sidebar:<18} show the Agents list"),
            format!("  {blocked} blocked on you; {working} still working"),
            String::new(),
        ]
    }
}

fn destinations_line(sidebar_visible: bool) -> String {
    if sidebar_visible {
        "These destinations stay clickable in the status bar and sidebar.".to_owned()
    } else {
        "These destinations stay clickable in the status bar.".to_owned()
    }
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

        let intro = begin_attach(&path).expect("claim intro");
        assert_eq!(intro.moment(), AttachMoment::Intro);
        assert!(intro.commit());
        assert!(begin_attach(&path).is_none());
        assert_eq!(after_detach(&path), Some(DETACH_NOTICE));
        assert_eq!(after_detach(&path), None);
        let returning = begin_attach(&path).expect("claim return");
        assert_eq!(returning.moment(), AttachMoment::Return);
        assert!(returning.commit());
        assert!(begin_attach(&path).is_none());
    }

    #[test]
    fn abandoned_delivery_is_retryable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = state_path_in(tmp.path());

        let intro = begin_attach(&path).expect("claim intro");
        assert_eq!(intro.moment(), AttachMoment::Intro);
        drop(intro);
        let retry = begin_attach(&path).expect("retry intro");
        assert_eq!(retry.moment(), AttachMoment::Intro);
        assert!(retry.commit());

        assert_eq!(after_detach(&path), Some(DETACH_NOTICE));
        let returning = begin_attach(&path).expect("claim return");
        assert_eq!(returning.moment(), AttachMoment::Return);
        drop(returning);
        assert_eq!(
            begin_attach(&path).expect("retry return").moment(),
            AttachMoment::Return
        );
    }

    #[test]
    fn separate_profile_directories_have_independent_journeys() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let installed = state_path_in(&tmp.path().join("phux"));
        let dev = state_path_in(&tmp.path().join("phux-dev"));

        assert!(begin_attach(&installed).expect("installed intro").commit());
        assert_eq!(after_detach(&installed), Some(DETACH_NOTICE));
        let dev_intro = begin_attach(&dev).expect("dev intro");
        assert_eq!(dev_intro.moment(), AttachMoment::Intro);
        assert!(dev_intro.commit());
        let installed_return = begin_attach(&installed).expect("installed return");
        assert_eq!(installed_return.moment(), AttachMoment::Return);
        assert!(installed_return.commit());
        assert!(begin_attach(&dev).is_none());
    }

    #[test]
    fn corrupt_and_future_state_fail_quiet() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = state_path_in(tmp.path());
        std::fs::write(&path, b"not json").expect("write corrupt state");
        assert!(begin_attach(&path).is_none());
        assert_eq!(after_detach(&path), None);

        std::fs::write(&path, br#"{"version":2,"stage":"intro-shown"}"#)
            .expect("write future state");
        assert!(begin_attach(&path).is_none());
    }

    #[test]
    fn unwritable_shape_fails_quiet() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let parent_file = tmp.path().join("not-a-directory");
        std::fs::write(&parent_file, b"x").expect("parent sentinel");
        let path = parent_file.join(STATE_FILE);
        assert!(begin_attach(&path).is_none());
        assert_eq!(after_detach(&path), None);
    }

    #[test]
    fn concurrent_claims_serialize() {
        use std::sync::{Arc, Barrier};

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = Arc::new(state_path_in(tmp.path()));
        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                begin_attach(&path).map(|claim| {
                    let moment = claim.moment();
                    assert!(claim.commit());
                    moment
                })
            }));
        }
        barrier.wait();
        let moments: Vec<_> = threads
            .into_iter()
            .filter_map(|thread| thread.join().expect("claim thread"))
            .collect();
        assert_eq!(moments, [AttachMoment::Intro]);
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
        let body = hint_lines(Some(&keys), true).join("\n");
        assert!(body.contains("C-b x"), "detach binding:\n{body}");
        assert!(body.contains("C-b p"), "palette binding:\n{body}");
        assert!(body.contains("C-b s"), "session binding:\n{body}");
        assert!(body.contains("C-b S"), "settings binding:\n{body}");
        assert!(body.contains("C-b ["), "copy binding:\n{body}");
        assert!(body.contains("Shift-drag"), "native selection:\n{body}");
        assert!(!body.contains("C-a d"), "must not advertise stale defaults");
        assert!(
            !body.contains("C-b b") && !body.contains("C-a b"),
            "open sidebar is not told to turn on:\n{body}"
        );
    }

    #[test]
    fn intro_names_the_agents_inbox_and_glyphs() {
        let keys = default_keybindings();
        let theme = crate::render::Theme::default();
        let blocked = crate::render::chrome::agent_badge(
            &theme,
            phux_client::agent_meta::AgentMetaState::Blocked,
            false,
            false,
        );
        let working = crate::render::chrome::agent_badge(
            &theme,
            phux_client::agent_meta::AgentMetaState::Working,
            false,
            false,
        );

        let visible = hint_lines(Some(&keys), true).join("\n");
        assert!(visible.contains("Agents"), "names the list:\n{visible}");
        assert!(
            visible.contains("fleet inbox"),
            "names the inbox:\n{visible}"
        );
        assert!(visible.contains(blocked.glyph), "blocked glyph:\n{visible}");
        assert!(visible.contains(working.glyph), "working glyph:\n{visible}");
        assert!(visible.contains("blocked"), "blocked word:\n{visible}");
        assert!(visible.contains("working"), "working word:\n{visible}");
        assert!(
            !visible.contains("C-a b"),
            "must not tell them to turn on a sidebar that is already open:\n{visible}"
        );
        assert!(
            visible.contains("sidebar"),
            "open strip still names the sidebar as a click target:\n{visible}"
        );

        let hidden = hint_lines(Some(&keys), false).join("\n");
        assert!(hidden.contains("C-a b"), "show-sidebar binding:\n{hidden}");
        assert!(hidden.contains("Agents"), "names the list:\n{hidden}");
        assert!(hidden.contains(blocked.glyph), "blocked glyph:\n{hidden}");
        assert!(hidden.contains(working.glyph), "working glyph:\n{hidden}");
        assert!(
            !hidden.contains("and sidebar"),
            "hidden strip is not advertised as already clickable:\n{hidden}"
        );
    }

    #[test]
    fn hidden_sidebar_guidance_tracks_rebound_toggle() {
        let mut keys = default_keybindings();
        keys.prefix = "C-b".to_owned();
        keys.prefix_table.retain(|_, action| {
            !matches!(
                action,
                phux_config::Action::Bare(name) if name == "toggle-sidebar"
            )
        });
        keys.prefix_table.insert(
            "w".to_owned(),
            phux_config::Action::Bare("toggle-sidebar".to_owned()),
        );
        let body = hint_lines(Some(&keys), false).join("\n");
        assert!(body.contains("C-b w"), "rebound sidebar:\n{body}");
        assert!(
            !body.contains("C-a b"),
            "must not advertise stale defaults:\n{body}"
        );
        assert!(body.contains("Agents"), "names the list:\n{body}");
    }
}
