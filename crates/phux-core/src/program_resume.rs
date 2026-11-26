//! A cold-start command recorded from OSC 7577.
//!
//! The per-pane token is not stored here. A value loaded from disk is checked
//! again, because the store is a file someone can edit.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What an accepted OSC 7577 report asked a later cold start to run.
///
/// The per-pane `OSC7577_TOKEN` is not part of this record. A stored value is
/// checked again by [`ProgramResume::is_valid`] on load: the store is a file
/// someone can edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramResume {
    /// The resume command. The first element is a bare name or an absolute path.
    pub argv: Vec<String>,
    /// Working directory from the report, or the owner's directory when the
    /// report omitted `cwd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Owning process from the report's `pid`.
    pub pid: u32,
    /// The owner's start time in a platform unit, so a reused pid is not this process.
    pub started: u64,
    /// User the owner ran as when the report was accepted.
    pub uid: u32,
    /// Unix seconds of the last report accepted for [`Self::pid`].
    pub time: u64,
    /// Set before the resume command is launched, so a crash does not retry it.
    #[serde(default)]
    pub tried: bool,
    /// The reporter was a same-uid direct child of the pane's shell.
    pub trusted: bool,
}

impl ProgramResume {
    /// Whether a stored record still matches the grammar a report had to pass.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.pid >= 1
            && self.time >= 1
            && valid_argv(&self.argv)
            && self.cwd.as_deref().is_none_or(valid_cwd)
    }

    /// The command as text a person can read: controls and bidi overrides are escaped.
    #[must_use]
    pub fn escaped_command(&self) -> String {
        escape_command(&self.argv)
    }
}

fn valid_argv(argv: &[String]) -> bool {
    let Some((program, rest)) = argv.split_first() else {
        return false;
    };
    valid_program(program) && rest.iter().all(|arg| !has_control(arg))
}

fn valid_program(program: &str) -> bool {
    if program.is_empty() || has_control(program) {
        return false;
    }
    if program.starts_with('/') {
        return program.len() > 1;
    }
    !program.starts_with('-') && !program.contains('/')
}

fn valid_cwd(cwd: &std::path::Path) -> bool {
    let Some(text) = cwd.to_str() else {
        return false;
    };
    text.starts_with('/') && !has_control(text)
}

fn has_control(text: &str) -> bool {
    text.chars().any(is_control_char)
}

const fn is_control_char(ch: char) -> bool {
    matches!(ch, '\u{0000}'..='\u{001F}' | '\u{007F}' | '\u{0080}'..='\u{009F}')
}

const fn is_bidi(ch: char) -> bool {
    matches!(
        ch,
        '\u{061C}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
    )
}

/// Join `argv` for display. Each element stays one argument; nothing is evaluated.
#[must_use]
pub fn escape_command(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| escape_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn escape_arg(arg: &str) -> String {
    let mut body = String::new();
    for ch in arg.chars() {
        match ch {
            '\\' => body.push_str("\\\\"),
            '\'' => body.push_str("'\\''"),
            ch if is_control_char(ch) || is_bidi(ch) => {
                let _ = write!(body, "\\u{{{:x}}}", u32::from(ch));
            }
            ch => body.push(ch),
        }
    }
    if body.is_empty() || body.contains([' ', '\t']) {
        format!("'{body}'")
    } else {
        body
    }
}

/// A change to a pane's OSC 7577 record. The token is never included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramResumeUpdate {
    /// Replace the record.
    Put(ProgramResume),
    /// Remove the record when it belongs to `pid`.
    Clear {
        /// The owner whose record goes away.
        pid: u32,
    },
}
