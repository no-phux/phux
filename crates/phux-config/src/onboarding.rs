//! Shared first-run progress for every client on this host.
//!
//! `<state_dir>/onboarding-progress.json` holds
//! `{"version":1,"generation":1,"step":"welcome"}`.
//! Steps are `welcome`, `look`, `agents`, `migrate`, `machines`, and `done`.
//! This is not the terminal UI's `onboarding.json` attach journey.
//!
//! A missing file, an unreadable file, an unknown version, an unknown
//! generation, an unknown step, or corrupt JSON reads as absent. A write
//! replaces the file by renaming a temp file over it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name inside the server state directory.
pub const FILE_NAME: &str = "onboarding-progress.json";

/// Schema version this build reads and writes. Any other version is absent.
pub const VERSION: u64 = 1;

/// Progress generation this build reads and writes. Any other generation is absent.
pub const GENERATION: u64 = 1;

/// One first-run step, shared by every client that reads this file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnboardingStep {
    /// The opening step.
    Welcome,
    /// Choosing a look.
    Look,
    /// Agents and, where the platform has them, permissions.
    Agents,
    /// Bringing a setup over from another agent.
    Migrate,
    /// Adding other machines.
    Machines,
    /// Finished. A stored `done` means first-run does not open on its own.
    Done,
}

#[derive(Debug, Serialize, Deserialize)]
struct Progress {
    version: u64,
    generation: u64,
    step: OnboardingStep,
}

/// The shared step in `state_dir`, when this build understands the file.
///
/// Missing, unreadable, unknown version, unknown generation, unknown step,
/// and corrupt JSON are absent.
#[must_use]
pub fn read(state_dir: &Path) -> Option<OnboardingStep> {
    let bytes = std::fs::read(state_dir.join(FILE_NAME)).ok()?;
    let progress = serde_json::from_slice::<Progress>(&bytes).ok()?;
    (progress.version == VERSION && progress.generation == GENERATION).then_some(progress.step)
}

/// Replace the shared step in `state_dir` with generation [`GENERATION`].
///
/// # Errors
///
/// The directory could not be created, or the temp file could not be
/// published. A failed publish leaves any previous file in place on Unix.
pub fn write(state_dir: &Path, step: OnboardingStep) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let dest = state_dir.join(FILE_NAME);
    let tmp = temp_path(state_dir);
    let bytes = serde_json::to_vec(&Progress {
        version: VERSION,
        generation: GENERATION,
        step,
    })
    .map_err(std::io::Error::other)?;
    let published = std::fs::write(&tmp, bytes).and_then(|()| rename_over(&tmp, &dest));
    if published.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    published
}

fn temp_path(state_dir: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    state_dir
        .join(FILE_NAME)
        .with_extension(format!("json.tmp-{}-{nanos}", std::process::id()))
}

/// `rename` replaces on Unix. Windows refuses when the destination exists.
fn rename_over(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    if to.exists() {
        std::fs::remove_file(to)?;
    }
    std::fs::rename(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEPS: [OnboardingStep; 6] = [
        OnboardingStep::Welcome,
        OnboardingStep::Look,
        OnboardingStep::Agents,
        OnboardingStep::Migrate,
        OnboardingStep::Machines,
        OnboardingStep::Done,
    ];

    #[test]
    fn each_step_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        for step in STEPS {
            write(dir.path(), step).unwrap();
            assert_eq!(read(dir.path()), Some(step));
        }
        let text = std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        assert_eq!(
            text, r#"{"version":1,"generation":1,"step":"done"}"#,
            "compact JSON, fixed field order"
        );
        assert!(!dir.path().join("onboarding.json").exists());
    }

    #[test]
    fn write_creates_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("state");
        write(&nested, OnboardingStep::Look).unwrap();
        assert_eq!(read(&nested), Some(OnboardingStep::Look));
    }

    #[test]
    fn unknown_or_corrupt_progress_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()), None, "missing file");
        let bodies = [
            "",
            "{",
            "[]",
            "null",
            r#"{"version":2,"generation":1,"step":"welcome"}"#,
            r#"{"version":1,"generation":2,"step":"welcome"}"#,
            r#"{"version":1,"generation":1,"step":"nope"}"#,
            r#"{"version":1,"generation":1,"step":"Welcome"}"#,
            r#"{"generation":1,"step":"welcome"}"#,
            r#"{"version":1,"step":"welcome"}"#,
        ];
        for body in bodies {
            std::fs::write(dir.path().join(FILE_NAME), body).unwrap();
            assert_eq!(read(dir.path()), None, "{body}");
        }
    }

    #[test]
    fn an_extra_field_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(FILE_NAME),
            r#"{"version":1,"generation":1,"step":"machines","extra":true}"#,
        )
        .unwrap();
        assert_eq!(read(dir.path()), Some(OnboardingStep::Machines));
    }
}
