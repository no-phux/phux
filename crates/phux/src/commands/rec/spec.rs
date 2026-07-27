//! Where a recording lands, and in what format.
//!
//! One planner, shared by both recording surfaces: the `--rec PATH` global
//! flag on the interactive attach path and the headless `phux rec TARGET -o
//! PATH` verb. Both hand a user-supplied path and an optional explicit format
//! to [`plan`] and get back a fully-resolved [`RecordSpec`]; neither has to
//! know the inference rules, so the two can never disagree about what
//! `demo.png` means.
//!
//! # Why the cast is always written
//!
//! Every export goes through an on-disk asciicast, even when the user asked
//! for a GIF. The `.cast` is the archival artifact — small, diffable, and
//! re-renderable at any fps with any idle limit — and the animation is a
//! derivative of it. For a GIF or an APNG the cast is an intermediate in the
//! temp directory, deleted once the render succeeds; if the render *fails* it
//! is deliberately retained and its path printed, so a capture that took
//! forty minutes of a live session is never lost to an encoder bug.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_record::render::OutputFormat;

use crate::commands::RecFormat;

/// A resolved recording plan: what to capture into, what to export to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordSpec {
    /// The artifact the user asked for. Carries the extension appended by
    /// [`plan`] when the given path had none.
    pub(crate) final_path: PathBuf,
    /// The format `final_path` is written in.
    pub(crate) format: OutputFormat,
    /// Where the asciicast is written. Equal to `final_path` when the user
    /// asked for a cast; a temp file otherwise.
    pub(crate) cast_path: PathBuf,
    /// Whether `cast_path` is ours to delete after a successful export.
    pub(crate) cast_is_temp: bool,
}

impl From<RecFormat> for OutputFormat {
    fn from(value: RecFormat) -> Self {
        match value {
            RecFormat::Cast => Self::Cast,
            RecFormat::Gif => Self::Gif,
            RecFormat::Apng => Self::Apng,
        }
    }
}

/// Resolve `out` plus an optional explicit format into a [`RecordSpec`].
///
/// The inference rules, in order:
///
/// 1. An explicit `--format` / `--rec-format` wins outright and the path is
///    left exactly as the user typed it — they named both halves, so there is
///    nothing left to infer and nothing we should be renaming.
/// 2. Otherwise the extension decides, case-insensitively: `cast`, `gif`, and
///    `png`/`apng`. `.png` maps to APNG because an *animated* PNG is what a
///    recording is; a still frame is not a thing this verb produces.
/// 3. A path with no extension at all becomes a GIF, and `.gif` is appended.
///    GIF is the shareable, embeddable default — it drops into a README, a
///    PR, or a chat window with no player and no plugin.
/// 4. An extension we do not recognize is an error naming the three we do.
///    Silently guessing GIF here would write a `demo.mp4` that is not an MP4.
///
/// Errors are reported on stderr and returned as the failure exit code, which
/// is the house shape for a CLI planner (see `commands::parse_selector`).
pub(crate) fn plan(out: &Path, explicit: Option<RecFormat>) -> Result<RecordSpec, ExitCode> {
    let extension = out
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase());

    let (format, final_path) = match (explicit, extension.as_deref()) {
        (Some(explicit), _) => (OutputFormat::from(explicit), out.to_path_buf()),
        (None, Some("cast")) => (OutputFormat::Cast, out.to_path_buf()),
        (None, Some("gif")) => (OutputFormat::Gif, out.to_path_buf()),
        (None, Some("png" | "apng")) => (OutputFormat::Apng, out.to_path_buf()),
        (None, None) => {
            // A value-taking flag will happily swallow the next word:
            // `phux --rec attach` parses as "record into a file named
            // attach", and the user sits there wondering why their session
            // never came up. Only an extension-less value can be confused
            // for a verb, so the guard costs nothing on every real path.
            if let Some(name) = subcommand_named(out) {
                eprintln!(
                    "phux: --rec wants an output path, not a subcommand; \
                     try `phux --rec demo.gif` or `phux {name} --rec demo.gif`"
                );
                return Err(ExitCode::FAILURE);
            }
            (OutputFormat::Gif, out.with_extension("gif"))
        }
        (None, Some(other)) => {
            eprintln!(
                "phux: rec: unknown output extension {other:?}; \
                 use .cast, .gif, or .png, or pass --format"
            );
            return Err(ExitCode::FAILURE);
        }
    };

    let (cast_path, cast_is_temp) = if format == OutputFormat::Cast {
        (final_path.clone(), false)
    } else {
        // Process-id-scoped so two concurrent recordings on one machine
        // cannot scribble over each other's intermediate.
        (
            std::env::temp_dir().join(format!("phux-rec-{}.cast", std::process::id())),
            true,
        )
    };

    Ok(RecordSpec {
        final_path,
        format,
        cast_path,
        cast_is_temp,
    })
}

/// The subcommand `out` names, if it names one.
///
/// Asks the real clap tree rather than a hand-kept list, so a verb added
/// tomorrow is guarded the day it lands. Only reached for an extension-less
/// path, so building the command tree is not on any hot path.
fn subcommand_named(out: &Path) -> Option<String> {
    use clap::CommandFactory as _;

    // Only a bare word can be a verb; `./attach` or `dir/attach` is
    // unambiguously a path the user typed on purpose.
    let candidate = match out.to_str() {
        Some(text) if !text.contains('/') => text,
        _ => return None,
    };
    let root = crate::Cli::command();
    root.get_subcommands()
        .find(|sub| {
            sub.get_name() == candidate || sub.get_all_aliases().any(|alias| alias == candidate)
        })
        .map(|sub| sub.get_name().to_owned())
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::{OutputFormat, RecFormat, RecordSpec, plan};
    use std::path::{Path, PathBuf};

    /// Plan `out` with no explicit format, asserting the planner accepted it.
    fn ok(out: &str) -> RecordSpec {
        plan(Path::new(out), None).expect("planner accepted the path")
    }

    #[test]
    fn infers_gif_from_extension() {
        let spec = ok("demo.gif");
        assert_eq!(spec.format, OutputFormat::Gif);
        assert_eq!(spec.final_path, PathBuf::from("demo.gif"));
    }

    #[test]
    fn infers_cast_from_extension() {
        let spec = ok("demo.cast");
        assert_eq!(spec.format, OutputFormat::Cast);
        assert_eq!(spec.final_path, PathBuf::from("demo.cast"));
    }

    #[test]
    fn infers_apng_from_png_and_from_apng() {
        // A recording is an animation, so a `.png` request is an APNG
        // request — there is no still-frame output from this verb.
        assert_eq!(ok("demo.png").format, OutputFormat::Apng);
        assert_eq!(ok("demo.apng").format, OutputFormat::Apng);
    }

    #[test]
    fn extension_match_is_case_insensitive() {
        assert_eq!(ok("DEMO.GIF").format, OutputFormat::Gif);
        assert_eq!(ok("Demo.Cast").format, OutputFormat::Cast);
        assert_eq!(ok("demo.PNG").format, OutputFormat::Apng);
    }

    #[test]
    fn appends_gif_when_the_path_has_no_extension() {
        let spec = ok("demo");
        assert_eq!(spec.format, OutputFormat::Gif);
        assert_eq!(
            spec.final_path,
            PathBuf::from("demo.gif"),
            "an extension-less path must gain the default format's extension, \
             not be written as a bare file"
        );
    }

    #[test]
    fn rejects_an_unknown_extension_with_an_actionable_message() {
        // The planner reports on stderr and returns the failure code; the
        // contract this pins is that it refuses rather than guessing.
        assert!(
            plan(Path::new("demo.mp4"), None).is_err(),
            "an unknown extension must not be silently rendered as something else"
        );
    }

    #[test]
    fn explicit_format_overrides_the_extension() {
        let spec = plan(Path::new("demo.gif"), Some(RecFormat::Cast)).expect("explicit format");
        assert_eq!(spec.format, OutputFormat::Cast);
        // Explicit means explicit: the path the user typed is the path we
        // write, extension mismatch and all.
        assert_eq!(spec.final_path, PathBuf::from("demo.gif"));
        assert_eq!(spec.cast_path, PathBuf::from("demo.gif"));
        assert!(!spec.cast_is_temp);
    }

    #[test]
    fn rejects_a_bare_subcommand_name_as_a_rec_path() {
        // `phux --rec attach` is the footgun of a value-taking global flag:
        // clap hands `attach` to `--rec` and the user gets no session.
        for verb in ["attach", "ls", "new"] {
            assert!(
                plan(Path::new(verb), None).is_err(),
                "`--rec {verb}` must be refused as a probable swallowed subcommand"
            );
        }
        // A path that merely contains a verb's name is fine, and so is one
        // that is qualified with a directory.
        assert!(plan(Path::new("attach.gif"), None).is_ok());
        assert!(plan(Path::new("./attach"), None).is_ok());
    }

    #[test]
    fn cast_format_uses_the_final_path_as_the_cast_path() {
        let spec = ok("demo.cast");
        assert_eq!(spec.cast_path, spec.final_path);
        assert!(
            !spec.cast_is_temp,
            "the user's own output file must never be deleted as an intermediate"
        );
    }

    #[test]
    fn non_cast_format_uses_a_temp_cast_path() {
        let spec = ok("demo.gif");
        assert_ne!(spec.cast_path, spec.final_path);
        assert!(spec.cast_is_temp);
        assert!(spec.cast_path.starts_with(std::env::temp_dir()));
        assert_eq!(
            spec.cast_path.extension().and_then(std::ffi::OsStr::to_str),
            Some("cast")
        );
        // Process-scoped, so two concurrent recordings do not collide.
        assert!(
            spec.cast_path
                .to_string_lossy()
                .contains(&std::process::id().to_string())
        );
    }
}
