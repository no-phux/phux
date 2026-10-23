//! Parse-failure reporting: the misplaced-flag hint and the key-material
//! guard and redaction.

use std::process::ExitCode;

use crate::help::{help_style, render_help_page};
use crate::{Cli, commands, json_output_requested, output};

fn flag_exists_on_any_verb(cmd: &usage::Command<'_>, long: &str) -> bool {
    cmd.flags.iter().any(|flag| flag.longs.contains(&long))
        || cmd
            .subcommands
            .iter()
            .any(|sub| flag_exists_on_any_verb(sub, long))
}

fn misplaced_scoped_flag(err: &usage::Error<'_, '_>) -> Option<String> {
    let usage::Error::UnknownFlag { token } = err else {
        return None;
    };
    let flag = String::from_utf8_lossy(token).into_owned();
    let long = flag.strip_prefix("--")?;
    flag_exists_on_any_verb(Cli::command(), long).then_some(flag)
}

/// Every verb path (`attach`, `host add`) that accepts `--long`.
fn verbs_with_flag(cmd: &usage::Command<'_>, long: &str, prefix: &str, out: &mut Vec<String>) {
    for sub in cmd.subcommands {
        let path = if prefix.is_empty() {
            sub.name.to_owned()
        } else {
            format!("{prefix} {}", sub.name)
        };
        if sub.flags.iter().any(|flag| flag.longs.contains(&long)) {
            out.push(path.clone());
        }
        verbs_with_flag(sub, long, &path, out);
    }
}

/// The teaching hint for a refused per-verb flag, keyed on where it was
/// typed. Before any verb it is a placement mistake (`phux --json ls`).
/// After a verb that does not take it, "place it after the verb" would be
/// wrong advice, so the hint names the verbs that do take it instead.
pub(crate) fn misplaced_flag_hint(err: &usage::Error<'_, '_>, words: &[String]) -> Option<String> {
    let flag = misplaced_scoped_flag(err)?;
    let long = flag.strip_prefix("--")?;
    let root = Cli::command();
    let at = words
        .iter()
        .position(|word| *word == flag || word.starts_with(&format!("{flag}=")))
        .unwrap_or(words.len());
    let verb = words.get(..at).unwrap_or_default().iter().find_map(|word| {
        root.subcommands
            .iter()
            .find(|sub| sub.name == word || sub.aliases.contains(&word.as_str()))
    });
    let Some(verb) = verb else {
        return Some(format!(
            "`{flag}` is set per verb, not on `phux` itself; place it after the verb: \
             `phux <verb> {flag} ...`"
        ));
    };
    let mut holders = Vec::new();
    verbs_with_flag(root, long, "", &mut holders);
    let shown: Vec<String> = holders
        .iter()
        .take(6)
        .map(|path| format!("`phux {path}`"))
        .collect();
    let more = if holders.len() > shown.len() {
        ", ..."
    } else {
        ""
    };
    Some(format!(
        "`phux {}` does not take `{flag}`; it belongs to {}{more}",
        verb.name,
        shown.join(", ")
    ))
}

/// Whether a `phux workload` invocation carries PEM or private-key material
/// in the words before any `--`. Only that verb is refused up front: it is
/// the one that handles key material, while other verbs legitimately carry
/// text that looks like it (input for a pane, a secret scanner's pattern).
pub(crate) fn workload_argv_carries_key_material(argv: &[std::ffi::OsString]) -> bool {
    let words: Vec<std::borrow::Cow<'_, str>> = argv
        .iter()
        .take_while(|word| word.as_os_str() != "--")
        .map(|word| word.to_string_lossy())
        .collect();
    let workload = words
        .iter()
        .find(|word| !word.starts_with('-'))
        .is_some_and(|verb| verb == "workload");
    workload
        && words
            .iter()
            .any(|word| word.contains("-----BEGIN") || word.contains("PRIVATE KEY"))
}

/// Shortest run of base64-alphabet characters [`redact_long_base64`] hides.
const REDACT_MIN_RUN: usize = 40;

/// Replace every run of [`REDACT_MIN_RUN`] or more characters of the base64
/// or base64url alphabet with `<redacted>`. usage-rs quotes the word it
/// refused, and a bare base64 line of a key, or a base64url secret such as a
/// JWK `d`, carries no PEM marker to catch (`docs/remote.md#who-can-connect`).
pub(crate) fn redact_long_base64(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_') {
            run.push(c);
        } else {
            flush_base64_run(&mut out, &mut run);
            out.push(c);
        }
    }
    flush_base64_run(&mut out, &mut run);
    out
}

fn flush_base64_run(out: &mut String, run: &mut String) {
    if run.len() >= REDACT_MIN_RUN {
        out.push_str("<redacted>");
    } else {
        out.push_str(run);
    }
    run.clear();
}

pub(crate) fn report_parse_error(argv: &[&std::ffi::OsStr], err: usage::Error<'_, '_>) -> ExitCode {
    let words: Vec<String> = argv
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    match err {
        usage::Error::Help { cmd, long } => {
            if let Some(page) = render_help_page(cmd, long, help_style()) {
                output::bytes(page.as_bytes());
            }
            ExitCode::SUCCESS
        }
        usage::Error::Version { .. } => {
            output::bytes(format!("phux {}\n", env!("PHUX_VERSION_LABEL")).as_bytes());
            ExitCode::SUCCESS
        }
        err => {
            // usage-rs owns parse explanations and suggestions. Adapt its fixed
            // error-header contract, never classify an arbitrary message. Keep
            // the usage/context block plain rather than coloring argv values.
            let rendered =
                redact_long_base64(&usage::render_failure_plain(Cli::spec(), argv, &err));
            let (header, context) = rendered.split_once('\n').unwrap_or((&rendered, ""));
            let message = header.strip_prefix("error: ").unwrap_or(header);
            let hint = misplaced_flag_hint(&err, &words);
            if json_output_requested() {
                return commands::json_err::emit(
                    true,
                    &commands::json_err::CliError::new(
                        commands::json_err::codes::USAGE_ERROR,
                        message,
                        hint.as_deref().unwrap_or_else(|| {
                            if context.trim().is_empty() {
                                "run `phux --help` for usage"
                            } else {
                                context.trim()
                            }
                        }),
                    ),
                    2,
                );
            }
            error!("{message}");
            // Parser-owned usage layout and argument underlines are not diagnostics.
            eprint!("{context}");
            if let Some(hint) = hint {
                hint!("{hint}");
            }
            ExitCode::from(2)
        }
    }
}
