//! `[restore]`: bringing panes back after a reboot or a crash.

use serde::{Deserialize, Serialize};

/// The `command` element a program's resume token replaces.
pub const RESUME_TOKEN_PLACEHOLDER: &str = "${PHUX_RESUME_TOKEN}";

/// `[restore]` table: bringing panes back after a reboot or a crash.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct RestoreCfg {
    /// Run `phux restore` when the server starts: relaunch every pane a
    /// server stop ended, and resume agents whose integration supports it.
    /// Nothing is typed into a relaunched program.
    pub on_start: bool,
    /// `[[restore.program]]`: the command a relaunch runs for a pane whose
    /// program handed over a resume token (OSC 7501 `resume=`), by `app`.
    /// A program with no entry here is never resumed.
    #[serde(deserialize_with = "programs")]
    pub program: Vec<RestoreProgram>,
}

/// One `[[restore.program]]` entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RestoreProgram {
    /// The OSC 7501 `app` this entry resumes.
    pub app: String,
    /// The argv to run, without a shell. Each element that is exactly
    /// [`RESUME_TOKEN_PLACEHOLDER`] becomes the token.
    pub command: Vec<String>,
}

impl RestoreCfg {
    /// The argv that resumes `app` with `token`, when the user listed one.
    #[must_use]
    pub fn resume_argv(&self, app: &str, token: &str) -> Option<Vec<String>> {
        let entry = self.program.iter().find(|entry| entry.app == app)?;
        Some(
            entry
                .command
                .iter()
                .map(|arg| {
                    if arg == RESUME_TOKEN_PLACEHOLDER {
                        token.to_owned()
                    } else {
                        arg.clone()
                    }
                })
                .collect(),
        )
    }
}

/// The token is one whole argument after the program name, or the entry is
/// refused: embedded in a longer string it could be read as source or an
/// option, and as the first element the pane would choose what runs.
fn programs<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Vec<RestoreProgram>, D::Error> {
    use serde::de::Error;

    let entries = Vec::<RestoreProgram>::deserialize(de)?;
    for (index, entry) in entries.iter().enumerate() {
        let app = &entry.app;
        let refuse = |why: &str| Err(D::Error::custom(format!("restore.program {app:?}: {why}")));
        // The OSC 7501 `app` grammar; anything else could never match.
        if !(1..=32).contains(&app.len())
            || !app
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
        {
            return refuse("`app` must be 1 to 32 bytes of [A-Za-z0-9_.+-]");
        }
        if entries
            .iter()
            .take(index)
            .any(|earlier| earlier.app == *app)
        {
            return refuse("`app` is listed twice");
        }
        let Some((program, args)) = entry.command.split_first() else {
            return refuse("`command` must not be empty");
        };
        if program.is_empty() || program.contains(RESUME_TOKEN_PLACEHOLDER) {
            return refuse("`command` must start with a program name");
        }
        if !args.iter().any(|arg| arg == RESUME_TOKEN_PLACEHOLDER)
            || args.iter().any(|arg| {
                arg != RESUME_TOKEN_PLACEHOLDER && arg.contains(RESUME_TOKEN_PLACEHOLDER)
            })
        {
            return refuse(&format!(
                "`command` needs {RESUME_TOKEN_PLACEHOLDER:?} as a whole argument, and nowhere else"
            ));
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::parse_with_defaults;

    fn load(program: &str) -> Result<super::RestoreCfg, String> {
        parse_with_defaults(program, Path::new("config.toml"))
            .map(|config| config.restore)
            .map_err(|err| err.to_string())
    }

    #[test]
    fn an_entry_substitutes_the_token_as_whole_arguments_only() {
        let restore = load(
            r#"
[[restore.program]]
app = "nvim"
command = ["nvim", "-S", "${PHUX_RESUME_TOKEN}", "$HOME", "${PHUX_RESUME_TOKEN}"]
"#,
        )
        .expect("valid");
        assert_eq!(
            restore.resume_argv("nvim", "a b; rm -rf /"),
            Some(
                ["nvim", "-S", "a b; rm -rf /", "$HOME", "a b; rm -rf /"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
        assert_eq!(restore.resume_argv("vim", "abc"), None);
        assert!(load("").expect("defaults").program.is_empty());
    }

    #[test]
    fn entries_that_could_not_resume_safely_are_refused_at_load() {
        for (entry, why) in [
            (
                r#"app = "", command = ["x", "${PHUX_RESUME_TOKEN}"]"#,
                "`app` must be",
            ),
            (
                r#"app = "my app", command = ["x", "${PHUX_RESUME_TOKEN}"]"#,
                "`app` must be",
            ),
            (r#"app = "x", command = []"#, "must not be empty"),
            (
                r#"app = "x", command = ["x", "--fixed"]"#,
                "as a whole argument",
            ),
            (
                r#"app = "x", command = ["${PHUX_RESUME_TOKEN}"]"#,
                "program name",
            ),
            (
                r#"app = "x", command = ["", "${PHUX_RESUME_TOKEN}"]"#,
                "program name",
            ),
            (
                r#"app = "x", command = ["sh", "-c", "run ${PHUX_RESUME_TOKEN}"]"#,
                "as a whole argument",
            ),
            (
                r#"app = "x", command = ["x", "${PHUX_RESUME_TOKEN}", "--id=${PHUX_RESUME_TOKEN}"]"#,
                "as a whole argument",
            ),
            (
                r#"app = "x", command = ["x", "${PHUX_RESUME_TOKEN}"], shell = true"#,
                "unknown field",
            ),
        ] {
            let err = load(&format!("restore.program = [{{ {entry} }}]")).expect_err(entry);
            assert!(err.contains(why), "{entry}: {err}");
        }
        let twice = r#"
[[restore.program]]
app = "x"
command = ["x", "${PHUX_RESUME_TOKEN}"]
[[restore.program]]
app = "x"
command = ["y", "${PHUX_RESUME_TOKEN}"]
"#;
        assert!(load(twice).expect_err("duplicate").contains("listed twice"));
    }
}
