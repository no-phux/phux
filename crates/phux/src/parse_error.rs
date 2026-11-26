//! Parse-failure reporting: the misplaced-flag hint and the key-material
//! guard and redaction.

use std::process::ExitCode;

use crate::help::{bare_required_flags, help_style, render_help_page};
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

/// The teaching hint for a refused per-verb flag, keyed on where it was
/// typed. Before any verb it is a placement mistake (`phux --json ls`).
/// After a verb, the hint names the exact command that refused it and,
/// when a relative under the same verb takes the flag, that relative; a
/// list of unrelated verbs that happen to take it would be noise.
pub(crate) fn misplaced_flag_hint(err: &usage::Error<'_, '_>, words: &[String]) -> Option<String> {
    let flag = misplaced_scoped_flag(err)?;
    let long = flag.strip_prefix("--")?;
    let at = words
        .iter()
        .position(|word| *word == flag || word.starts_with(&format!("{flag}=")))
        .unwrap_or(words.len());
    let path = typed_path(words.get(..at).unwrap_or_default());
    if path.is_empty() {
        return Some(format!(
            "`{flag}` is set per verb, not on `phux` itself; place it after the verb: \
             `phux <verb> {flag} ...`"
        ));
    }
    let names: Vec<&str> = path.iter().map(|(name, _)| *name).collect();
    let typed = names.join(" ");
    Some(flag_hint(&typed, &flag, &relatives_with_flag(&path, long)))
}

/// The commands closest to the typed one that take `--long`: its parent
/// and the parent's other children (a bare verb's own children). Anything
/// farther is a different task, not a near miss.
fn relatives_with_flag(path: &[(&str, &usage::Command<'_>)], long: &str) -> Vec<String> {
    let takes = |cmd: &usage::Command<'_>| cmd.flags.iter().any(|f| f.longs.contains(&long));
    let anchor_len = path.len().saturating_sub(1).max(1);
    let anchor_path = path[..anchor_len]
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(" ");
    let anchor = path[anchor_len - 1].1;
    let typed = path.last().map(|(_, cmd)| *cmd);
    let mut relatives = Vec::new();
    if anchor_len < path.len() && takes(anchor) {
        relatives.push(anchor_path.clone());
    }
    for sub in anchor.subcommands {
        if takes(sub) && !typed.is_some_and(|typed| std::ptr::eq(typed, *sub)) {
            relatives.push(format!("{anchor_path} {}", sub.name));
        }
    }
    relatives
}

/// The command words a user typed, resolved through the tree: each one's
/// declared name (an alias resolved) with its command. Flags, and values
/// that name no subcommand, are passed over.
fn typed_path(words: &[String]) -> Vec<(&'static str, &'static usage::Command<'static>)> {
    let mut path = Vec::new();
    let mut cmd = Cli::command();
    for word in words {
        let found = cmd
            .subcommands
            .iter()
            .find(|sub| sub.name == word || sub.aliases.contains(&word.as_str()));
        if let Some(&sub) = found {
            path.push((sub.name, sub));
            cmd = sub;
        }
    }
    path
}

/// "`phux relay pair` does not take `--json`; see `phux relay pair --help`",
/// naming up to three relatives that do take it.
fn flag_hint(typed: &str, flag: &str, relatives: &[String]) -> String {
    let help = format!("see `phux {typed} --help`");
    if relatives.is_empty() {
        return format!("`phux {typed}` does not take `{flag}`; {help}");
    }
    let shown: Vec<String> = relatives
        .iter()
        .take(3)
        .map(|path| format!("`phux {path}`"))
        .collect();
    let more = if relatives.len() > shown.len() {
        ", ..."
    } else {
        ""
    };
    let verb = if shown.len() == 1 && more.is_empty() {
        "takes"
    } else {
        "take"
    };
    format!(
        "`phux {typed}` does not take `{flag}`; {} {verb} it; {help}",
        shown.join(", ") + more
    )
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

/// A parser header that ends in `:` lists its subject on the indented lines
/// after it ("required arguments were not provided:\n  <TARGET>"). A JSON
/// `message` is one line, so fold that list into it and out of the context.
pub(crate) fn complete_failure_header<'a>(header: &str, context: &'a str) -> (String, &'a str) {
    let Some(stem) = header.strip_suffix(':') else {
        return (header.to_owned(), context);
    };
    let mut items = Vec::new();
    let mut rest = context;
    while let Some(line) = rest.lines().next() {
        if !line.starts_with(char::is_whitespace) || line.trim().is_empty() {
            break;
        }
        items.push(line.trim());
        rest = rest.get(line.len() + 1..).unwrap_or("");
    }
    if items.is_empty() {
        return (header.to_owned(), context);
    }
    (format!("{stem}: {}", items.join(", ")), rest)
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
            let rendered = bare_required_flags(&redact_long_base64(&usage::render_failure_plain(
                Cli::spec(),
                argv,
                &err,
            )));
            let (header, context) = rendered.split_once('\n').unwrap_or((&rendered, ""));
            let message = header.strip_prefix("error: ").unwrap_or(header);
            let hint = misplaced_flag_hint(&err, &words);
            let (json_message, json_context) = complete_failure_header(message, context);
            if json_output_requested() {
                return commands::json_err::emit(
                    true,
                    &commands::json_err::CliError::new(
                        commands::json_err::codes::USAGE_ERROR,
                        json_message,
                        hint.as_deref().unwrap_or_else(|| {
                            if json_context.trim().is_empty() {
                                "run `phux --help` for usage"
                            } else {
                                json_context.trim()
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
