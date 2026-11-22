//! `phux ls SESSION`: one session's terminals, exceptions first.

use std::process::ExitCode;

use phux_client::glance::{self, MemberGlance, Sources};
use phux_client::org::KeyNames;

use phux_protocol::wire::info::SessionInfo;

use crate::commands::json_err::{self, CliError, codes};

use super::glance_rows::glance_text;
use super::lines::{format_row, with_key};

/// `phux ls SESSION`: the session's glance line (its `-l` line with
/// `detail`), then one line per terminal, exceptions first.
pub(super) fn print_one_session(sources: Sources<'_>, name: &str, detail: bool) -> ExitCode {
    let Some(session) = sources.snapshot.sessions.iter().find(|s| s.name == name) else {
        return json_err::emit(
            false,
            &CliError::new(
                codes::NO_SUCH_TARGET,
                format!("no such session: {name}"),
                "run `phux ls` to see sessions",
            ),
            crate::exit_codes::EXIT_FAILURE,
        );
    };
    for line in one_session_lines(sources, session, detail) {
        outln!("{line}");
    }
    ExitCode::SUCCESS
}

pub(super) fn one_session_lines(
    sources: Sources<'_>,
    session: &SessionInfo,
    detail: bool,
) -> Vec<String> {
    let names = sources.organization.map(KeyNames::of_inventory);
    let header = if detail {
        let row = phux_client::session_list::session_json(
            sources.snapshot,
            sources.organization,
            session,
        );
        let key = sources
            .organization
            .and_then(|inventory| inventory.session_by_live(session.id))
            .zip(names.as_ref())
            .map(|(record, names)| names.display(*record.key.as_bytes()));
        with_key(format_row(&row), key.as_deref())
    } else {
        glance_text(&glance::session_glance(sources, session))
    };
    let members = glance::members(sources, session);
    if members.is_empty() {
        return vec![header, "  (no terminals)".to_owned()];
    }
    let rows: Vec<Vec<String>> = members
        .iter()
        .map(|member| member_cells(member, detail, names.as_ref()))
        .collect();
    let mut lines = vec![header];
    lines.extend(columns(&rows).into_iter().map(|line| format!("  {line}")));
    lines
}

/// One terminal's cells: selector, state, agent, title, directory; with
/// `detail` also its key and its revision and run.
pub(super) fn member_cells(
    member: &MemberGlance,
    detail: bool,
    names: Option<&KeyNames>,
) -> Vec<String> {
    let mut cells = vec![member.selector()];
    if detail {
        let key = member.key.zip(names);
        cells.push(key.map_or_else(String::new, |(key, names)| names.display(*key.as_bytes())));
    }
    cells.push(member.status.clone());
    cells.push(member.agent.as_deref().map(printable).unwrap_or_default());
    cells.push(member.title.as_deref().map(printable).unwrap_or_default());
    cells.push(member.cwd.as_deref().map(home_relative).unwrap_or_default());
    if detail {
        cells.push(member.record.clone().unwrap_or_default());
    }
    cells
}

/// `rows` as left-aligned columns two spaces apart, dropping columns empty
/// in every row and trailing blanks.
pub(super) fn columns(rows: &[Vec<String>]) -> Vec<String> {
    let count = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..count)
        .map(|col| {
            rows.iter()
                .map(|row| row.get(col).map_or(0, |cell| cell.chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.iter()
        .map(|row| {
            let mut line = String::new();
            for (cell, width) in row.iter().zip(&widths) {
                if *width == 0 {
                    continue;
                }
                if !line.is_empty() {
                    line.push_str("  ");
                }
                line.push_str(cell);
                line.extend(std::iter::repeat_n(' ', width - cell.chars().count()));
            }
            line.trim_end().to_owned()
        })
        .collect()
}

/// Program-set text without control or bidi characters, so it cannot
/// rewrite the listing around it.
pub(super) fn printable(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() && !phux_tui::render::is_bidi_control(*ch))
        .collect()
}

/// `dir` with the home directory spelled `~`.
pub(super) fn home_relative(dir: &str) -> String {
    let dir = printable(dir);
    let home = std::env::var("HOME").unwrap_or_default();
    if home.is_empty() {
        return dir;
    }
    if dir == home {
        return "~".to_owned();
    }
    match dir.strip_prefix(&home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => dir,
    }
}
