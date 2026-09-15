//! `phux tag` — read and write a Terminal's L3 tags (`phux-f8wi`, ADR-0027).
//!
//! Tags are freeform strings stored as L3 metadata under the conventional
//! key `RESOURCE_TAGS_KEY` (`phux.tags/v1`), scoped to a `ResourceId`. Once a
//! Terminal is tagged, the `#tag` selector ([`crate::selector`]) addresses
//! every Terminal carrying that tag — the read side this command writes.
//! The wire round trips (connect, `GET_STATE` + tag index, `SET_METADATA` +
//! confirming `GET_METADATA`) live in [`phux_client::tags`].

use std::process::ExitCode;

use phux_client::selector::{self, TagIndex};
use phux_client::tags::TagWriteOutcome;
use phux_protocol::ids::ResourceId;
use phux_server::runtime::default_socket_path;

use crate::commands::json_err::{self, CliError, codes};
use crate::commands::{TagAction, cli_runtime, partial};

/// Dispatch `phux tag <action>`.
pub(crate) fn run_tag(action: &TagAction, socket: Option<std::path::PathBuf>) -> ExitCode {
    let (target, json) = match action {
        TagAction::Ls { target, json }
        | TagAction::Add { target, json, .. }
        | TagAction::Rm { target, json, .. } => (target, json.json),
    };
    let selector = match selector::parse(target) {
        Ok(sel) => sel,
        Err(err) => {
            return json_err::emit(
                json,
                &CliError::new(
                    codes::INVALID_SELECTOR,
                    format!("invalid target '{target}': {err}"),
                    "selector forms: NAME, NAME:WIN, NAME:WIN.PANE, @ID, `.`, #TAG",
                ),
                1,
            );
        }
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        let phux_client::tags::TagSession {
            mut conn,
            snapshot: _snapshot,
            degradation,
            index,
            targets,
        } = match phux_client::tags::prepare(&socket_path, &selector).await {
            Ok(session) => session,
            Err(err) => return json_err::report_no_server(json, &err, &socket_path, "tag"),
        };
        if targets.is_empty() {
            // Every `phux tag` target is Terminal-scoped, and `panes` is the
            // list a hub aggregates. So an empty match against a degraded
            // snapshot is unresolved, not absent — and `tag ls` reporting
            // "no such target" for a pane sitting on a temporarily
            // unreachable satellite is the exact confusion this splits apart.
            // Under `--json` the same split lands in `error.code`
            // (`no_such_target` vs `partial_view`), same exit codes.
            return partial::report_target_miss_for(json, Some(target), &degradation);
        }
        // A `#tag` set resolved against a partial fleet is a subset of the
        // real one; the writes below will land on that subset only. Under
        // `--json` this stays a prose stderr warning ahead of the document,
        // per the contract's warnings rule.
        partial::warn_partial_view("tag", &degradation);

        let code = match action {
            TagAction::Ls { .. } => {
                let rows: Vec<(ResourceId, Vec<String>)> = targets
                    .iter()
                    .map(|id| (id.clone(), index.get(id).cloned().unwrap_or_default()))
                    .collect();
                print_rows(json, &rows)
            }
            TagAction::Add { tags, .. } => {
                let wanted = phux_client::tags::normalize(tags);
                edit_tags(&mut conn, &targets, &index, &socket_path, json, |cur| {
                    for t in &wanted {
                        if !cur.iter().any(|e| e == t) {
                            cur.push(t.clone());
                        }
                    }
                })
                .await
            }
            TagAction::Rm { tags, .. } => {
                let unwanted = phux_client::tags::normalize(tags);
                edit_tags(&mut conn, &targets, &index, &socket_path, json, |cur| {
                    cur.retain(|e| !unwanted.iter().any(|u| u == e));
                })
                .await
            }
        };
        drop(conn);
        code
    })
}

/// Print the per-Terminal tag rows: the human view (one `SELECTOR\tTAGS`
/// line per Terminal) or, under `--json`, the stable document
/// [`tags_document`] pins.
fn print_rows(json: bool, rows: &[(ResourceId, Vec<String>)]) -> ExitCode {
    if json {
        return match serde_json::to_string_pretty(&tags_document(rows)) {
            Ok(rendered) => {
                outln!("{rendered}");
                ExitCode::SUCCESS
            }
            Err(err) => json_err::emit(
                true,
                &CliError::new(
                    codes::JSON_SERIALIZE,
                    err.to_string(),
                    "this is a phux bug; run `phux doctor` and report it",
                ),
                1,
            ),
        };
    }
    for (id, tags) in rows {
        outln!("{}", render_tags(id, tags));
    }
    ExitCode::SUCCESS
}

/// The `phux tag --json` document, shared by `ls` and the confirmed
/// post-write state of `add` / `rm` (documented in
/// `docs/consumers/agents.md` §4.17).
///
/// One row per resolved Terminal: `terminal` is the canonical, reusable
/// selector (`@7`, or `host/@7` for a satellite pane) and `tags` is the
/// Terminal's full tag list — for the edit verbs, as read back from the
/// server after the write, never echoed from the request.
fn tags_document(rows: &[(ResourceId, Vec<String>)]) -> serde_json::Value {
    let terminals: Vec<_> = rows
        .iter()
        .map(|(id, tags)| {
            serde_json::json!({
                "terminal": selector::format_terminal_id(id),
                "tags": tags,
            })
        })
        .collect();
    serde_json::json!({
        "schema_version": 1,
        "terminals": terminals,
    })
}

/// One tag output line, prefixed by a canonical, reusable Terminal selector.
fn render_tags(id: &ResourceId, tags: &[String]) -> String {
    format!("{}\t{}", selector::format_terminal_id(id), tags.join(" "))
}

/// Read every `targets` Terminal's current tags from `index`, apply `mutate`,
/// and write the result back via [`phux_client::tags::write_tags`].
async fn edit_tags<F: Fn(&mut Vec<String>)>(
    conn: &mut phux_client::attach::connection::Connection,
    targets: &[ResourceId],
    index: &TagIndex,
    socket_path: &std::path::Path,
    json: bool,
    mutate: F,
) -> ExitCode {
    // Under `--json` the confirmed rows are buffered and emitted as one
    // document after every write lands: a partial document on stdout would
    // break the "one object or nothing" hygiene contract, and any failure
    // below leaves stdout empty with the error line on stderr.
    let mut rows: Vec<(ResourceId, Vec<String>)> = Vec::with_capacity(targets.len());
    let mut req: u32 = 100;
    for id in targets {
        let mut cur = index.get(id).cloned().unwrap_or_default();
        mutate(&mut cur);
        cur.sort();
        cur.dedup();
        req += 1;
        let (outcome, degradation) = match phux_client::tags::write_tags(conn, req, id, &cur).await
        {
            Ok(v) => v,
            Err(err) => return json_err::report_no_server(json, &err, socket_path, "tag"),
        };
        req += 1;
        for message in degradation.notices() {
            eprintln!("phux: warning: partial results — {message}");
        }
        let confirmed = match outcome {
            TagWriteOutcome::Confirmed(tags) => tags,
            TagWriteOutcome::Refused(refusal) => {
                return json_err::emit(
                    json,
                    &CliError::new(
                        codes::TRANSPORT,
                        format!(
                            "tag write to {} could not be confirmed: server refused the read: {refusal}",
                            selector::format_terminal_id(id),
                        ),
                        "run `phux doctor` for a health check",
                    ),
                    1,
                );
            }
        };
        if json {
            rows.push((id.clone(), confirmed));
        } else {
            outln!("{}", render_tags(id, &confirmed));
        }
    }
    if json {
        return print_rows(true, &rows);
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]

    use super::*;

    #[test]
    fn satellite_tag_output_uses_canonical_selector() {
        assert_eq!(
            render_tags(
                &ResourceId::satellite("region/@build", 7),
                &["ci".to_owned(), "urgent".to_owned()],
            ),
            "region/@build/@7\tci urgent"
        );
    }

    /// The `phux tag --json` document, pinned (phux-i0e8.8.3, documented in
    /// agents.md §4.17): `schema_version` 1 and one row per Terminal with
    /// the canonical selector under `terminal` and the full tag list under
    /// `tags`. `ls` and the confirmed post-write state of `add`/`rm` share
    /// this one shape, so a consumer parses one document for all three.
    #[test]
    fn tags_document_pins_the_shape() {
        let rows = vec![
            (
                ResourceId::local(7),
                vec!["build".to_owned(), "ci".to_owned()],
            ),
            (ResourceId::satellite("edge", 3), Vec::new()),
        ];
        let doc = tags_document(&rows);
        assert_eq!(doc["schema_version"], 1);
        let terminals = doc["terminals"].as_array().unwrap();
        assert_eq!(terminals.len(), 2);
        assert_eq!(terminals[0]["terminal"], "@7");
        assert_eq!(terminals[0]["tags"][0], "build");
        assert_eq!(terminals[0]["tags"][1], "ci");
        assert_eq!(terminals[1]["terminal"], "edge/@3");
        assert_eq!(
            terminals[1]["tags"].as_array().map(Vec::len),
            Some(0),
            "an untagged Terminal is an empty list, never an absent key"
        );
        // Exactly the two top-level keys, so additive growth is deliberate.
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(2));
    }
}
