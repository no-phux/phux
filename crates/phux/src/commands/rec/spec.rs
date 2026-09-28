//! Where a recording lands, and in what format: one planner ([`plan`]) for
//! both `--rec PATH` and `phux rec -o PATH`.
//!
//! Every export goes through an on-disk asciicast (the archival,
//! re-renderable artifact). For GIF/APNG it is a temp intermediate, deleted
//! after a successful render and kept, with its path printed, when the render
//! fails.

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

/// Resolve `out` plus an optional explicit format into a [`RecordSpec`]:
///
/// 1. An explicit format wins and the path is left as typed.
/// 2. Otherwise the extension decides, case-insensitively: `cast`, `gif`, or
///    `png`/`apng` (APNG).
/// 3. No extension means GIF, with `.gif` appended.
/// 4. An unknown extension is an error naming the three.
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
            // `phux --rec attach` would record into a file named `attach`; an
            // extension-less value naming a verb is refused.
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
            // The one deliberate `{other:?}` outside test code
            // (phux-i0e8.7.3): `other` is the user's own file extension — a
            // string, not a wire enum — and Debug formatting here is plain
            // quoting, so `demo.mp4` reports `unknown output extension "mp4"`.
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

/// The subcommand `out` names, if any, asked of the real command tree.
fn subcommand_named(out: &Path) -> Option<String> {
    // Only a bare word can be a verb; `./attach` or `dir/attach` is
    // unambiguously a path the user typed on purpose.
    let candidate = match out.to_str() {
        Some(text) if !text.contains('/') => text,
        _ => return None,
    };
    crate::Cli::spec()
        .root
        .subcommands
        .iter()
        .find(|sub| sub.cmd.name == candidate || sub.cmd.aliases.contains(&candidate))
        .map(|sub| sub.cmd.name.to_owned())
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
