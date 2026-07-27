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

use crate::commands::{TagAction, cli_runtime, report_no_server};

/// Dispatch `phux tag <action>`.
pub(crate) fn run_tag(action: &TagAction, socket: Option<std::path::PathBuf>) -> ExitCode {
    let target = match action {
        TagAction::Ls { target } | TagAction::Add { target, .. } | TagAction::Rm { target, .. } => {
            target
        }
    };
    let selector = match selector::parse(target) {
        Ok(sel) => sel,
        Err(err) => {
            eprintln!("phux: invalid target '{target}': {err}");
            return ExitCode::FAILURE;
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
            Err(err) => return report_no_server(&err, &socket_path, "tag"),
        };
        let snapshot = match phux_client::state::get_state_on(&mut conn).await {
            Ok(snapshot) => snapshot,
            Err(err) => return report_no_server(&err, &socket_path, "tag"),
        };

        // `phux tag` resolves the target itself (it may be a `#tag` selector,
        // e.g. re-tagging a set), so it goes through the tag-aware resolver.
        let index = phux_client::state::fetch_tag_index(&mut conn, &snapshot).await;
        let targets = selector::resolve_with_tags(&selector, &snapshot, &index);
        if targets.is_empty() {
            eprintln!("phux: no such target: {target}");
            return ExitCode::FAILURE;
        }

        match action {
            TagAction::Ls { .. } => {
                for id in &targets {
                    let tags = index.get(id).cloned().unwrap_or_default();
                    outln!("{}", render_tags(id, &tags));
                }
                ExitCode::SUCCESS
            }
            TagAction::Add { tags, .. } => {
                let wanted = normalize(tags);
                edit_tags(&mut conn, &targets, &index, &socket_path, |cur| {
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
                edit_tags(&mut conn, &targets, &index, &socket_path, |cur| {
                    cur.retain(|e| !unwanted.iter().any(|u| u == e));
                })
                .await
            }
        }
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
    mutate: F,
) -> ExitCode {
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
            return report_no_server(&err, socket_path, "tag");
        }
        req += 1;
        let get_id = req;
        if let Err(err) = conn
            .send(&FrameKind::GetMetadata {
                request_id: get_id,
                scope: Scope::Terminal(id.clone()),
                key: TERMINAL_TAGS_KEY.to_owned(),
            })
            .await
        {
            return report_no_server(&err, socket_path, "tag");
        }
        // Drain to the matching reply (proves the prior SET landed).
        let confirmed = loop {
            match conn.recv().await {
                Ok(FrameKind::MetadataValue { request_id, value }) if request_id == get_id => {
                    break value
                        .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
                        .unwrap_or_default();
                }
                Ok(_) => {}
                Err(err) => return report_no_server(&err, socket_path, "tag"),
            }
        };
        outln!("{}", render_tags(id, &confirmed));
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
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
}
