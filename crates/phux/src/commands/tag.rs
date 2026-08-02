//! `phux tag` — read and write a Terminal's L3 tags (`phux-f8wi`, ADR-0027).
//!
//! Tags are freeform strings stored as L3 metadata under the conventional
//! key [`TERMINAL_TAGS_KEY`] (`phux.tags/v1`), scoped to a `TerminalId`. The
//! value is a UTF-8 JSON array of tag strings; the server stores the bytes
//! opaquely ([`docs/spec/L3.md`](../../../docs/spec/L3.md) §3.6). Once a
//! Terminal is tagged, the `#tag` selector ([`crate::selector`]) addresses
//! every Terminal carrying that tag — the read side this command writes.

use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_client::selector::{self, TagIndex};
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{FrameKind, Scope, TERMINAL_TAGS_KEY};
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
        let mut conn = match Connection::connect(&socket_path).await {
            Ok(conn) => conn,
            Err(err) => return json_err::report_no_server(json, &err, &socket_path, "tag"),
        };
        let (snapshot, degradation) = match phux_client::state::get_state_on(&mut conn).await {
            Ok(view) => view.into_parts(),
            Err(err) => return json_err::report_no_server(json, &err, &socket_path, "tag"),
        };

        // `phux tag` resolves the target itself (it may be a `#tag` selector,
        // e.g. re-tagging a set), so it goes through the tag-aware resolver.
        let index = phux_client::state::fetch_tag_index(&mut conn, &snapshot).await;
        let targets = selector::resolve_with_tags(&selector, &snapshot, &index);
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

        match action {
            TagAction::Ls { .. } => {
                let rows: Vec<(TerminalId, Vec<String>)> = targets
                    .iter()
                    .map(|id| (id.clone(), index.get(id).cloned().unwrap_or_default()))
                    .collect();
                print_rows(json, &rows)
            }
            TagAction::Add { tags, .. } => {
                let wanted = normalize(tags);
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
                let unwanted = normalize(tags);
                edit_tags(&mut conn, &targets, &index, &socket_path, json, |cur| {
                    cur.retain(|e| !unwanted.iter().any(|u| u == e));
                })
                .await
            }
        }
    })
}

/// Print the per-Terminal tag rows: the human view (one `SELECTOR\tTAGS`
/// line per Terminal) or, under `--json`, the stable document
/// [`tags_document`] pins.
fn print_rows(json: bool, rows: &[(TerminalId, Vec<String>)]) -> ExitCode {
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
fn tags_document(rows: &[(TerminalId, Vec<String>)]) -> serde_json::Value {
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

/// Strip an optional leading `#` from each supplied tag and drop empties /
/// duplicates, so `phux tag add x #x` and `phux tag add x` are equivalent.
fn normalize(tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = t.strip_prefix('#').unwrap_or(t).trim();
        if !t.is_empty() && !out.iter().any(|e| e == t) {
            out.push(t.to_owned());
        }
    }
    out
}

/// One tag output line, prefixed by a canonical, reusable Terminal selector.
fn render_tags(id: &TerminalId, tags: &[String]) -> String {
    format!("{}\t{}", selector::format_terminal_id(id), tags.join(" "))
}

/// Read every `targets` Terminal's current tags from `index`, apply `mutate`,
/// and write the result back via `SET_METADATA`, then a `GET_METADATA`
/// round-trip per Terminal.
///
/// The trailing GET is load-bearing, not cosmetic: `SET_METADATA` carries no
/// reply, so without a following round-trip the client could exit and close
/// the socket before the server reads the SET frame, dropping the write
/// (the same reason `phux new` GETs after its create SET). Frames are ordered
/// on the one connection, so the GET's reply proves the SET was applied; we
/// print that confirmed value.
async fn edit_tags<F: Fn(&mut Vec<String>)>(
    conn: &mut Connection,
    targets: &[TerminalId],
    index: &TagIndex,
    socket_path: &std::path::Path,
    json: bool,
    mutate: F,
) -> ExitCode {
    // Under `--json` the confirmed rows are buffered and emitted as one
    // document after every write lands: a partial document on stdout would
    // break the "one object or nothing" hygiene contract, and any failure
    // below leaves stdout empty with the error line on stderr.
    let mut rows: Vec<(TerminalId, Vec<String>)> = Vec::with_capacity(targets.len());
    let mut req: u32 = 100;
    for id in targets {
        let mut cur = index.get(id).cloned().unwrap_or_default();
        mutate(&mut cur);
        cur.sort();
        cur.dedup();
        let value = serde_json::to_vec(&cur).unwrap_or_else(|_| b"[]".to_vec());
        req += 1;
        if let Err(err) = conn
            .send(&FrameKind::SetMetadata {
                request_id: req,
                scope: Scope::Terminal(id.clone()),
                key: TERMINAL_TAGS_KEY.to_owned(),
                value,
            })
            .await
        {
            return json_err::report_no_server(json, &err, socket_path, "tag");
        }
        req += 1;
        // The confirming read (proves the prior SET landed). Routed through
        // `request_metadata` rather than a hand-rolled wait: the loop that
        // used to live here matched METADATA_VALUE and dropped everything
        // else, so a server that refused the read with a correlated ERROR
        // (`proto.md` §9) left `phux tag add` hung with no output and no exit.
        let reply = match conn
            .request_metadata(
                req,
                Scope::Terminal(id.clone()),
                TERMINAL_TAGS_KEY.to_owned(),
            )
            .await
        {
            Ok(reply) => reply,
            Err(err) => return json_err::report_no_server(json, &err, socket_path, "tag"),
        };
        let (answer, interleaved) = reply.into_parts();
        for message in phux_client::state::degradation_notices(&interleaved) {
            eprintln!("phux: warning: partial results — {message}");
        }
        let confirmed = match answer {
            Ok(value) => value
                .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
                .unwrap_or_default(),
            Err(refusal) => {
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
                &TerminalId::satellite("region/@build", 7),
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
                TerminalId::local(7),
                vec!["build".to_owned(), "ci".to_owned()],
            ),
            (TerminalId::satellite("edge", 3), Vec::new()),
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
