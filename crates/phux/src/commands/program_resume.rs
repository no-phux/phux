//! The escaped OSC 7577 commands a server is holding. The index is global
//! metadata; it never contains the per-pane token.

use std::collections::BTreeMap;

use phux_client::attach::connection::Connection;
use phux_protocol::wire::frame::{PROGRAM_RESUME_INDEX_KEY, Scope};

/// One stored resume command, already escaped by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResumeLine {
    /// The command, escaped for display.
    pub command: String,
    /// The reporter was a same-uid direct child of the pane's shell.
    pub trusted: bool,
    /// A resume was already attempted.
    pub tried: bool,
}

impl ResumeLine {
    /// The text `phux ls -l` and `phux restore --dry-run` print.
    #[must_use]
    pub(crate) fn display(&self) -> String {
        let kind = if self.tried {
            "resume tried"
        } else if self.trusted {
            "resume"
        } else {
            "resume untrusted"
        };
        format!("{kind}: {}", self.command)
    }
}

/// Read the server's resume index. An absent key or a bad document is empty.
pub(crate) async fn fetch(conn: &mut Connection) -> BTreeMap<String, ResumeLine> {
    let Ok(reply) = conn
        .request_metadata(0x7577, Scope::Global, PROGRAM_RESUME_INDEX_KEY.to_owned())
        .await
    else {
        return BTreeMap::new();
    };
    let (answer, _) = reply.into_parts();
    let Ok(Some(bytes)) = answer else {
        return BTreeMap::new();
    };
    parse(&bytes)
}

fn parse(bytes: &[u8]) -> BTreeMap<String, ResumeLine> {
    let Ok(value) = serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(bytes)
    else {
        return BTreeMap::new();
    };
    value
        .into_iter()
        .filter_map(|(key, entry)| {
            let command = entry.get("command").and_then(|value| value.as_str())?;
            Some((
                key,
                ResumeLine {
                    command: command.to_owned(),
                    trusted: entry
                        .get("trusted")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    tried: entry
                        .get("tried")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_escapes_nothing_further_and_names_an_untrusted_record() {
        let line = ResumeLine {
            command: "nvim -S '\\u{202e}evil'".into(),
            trusted: false,
            tried: false,
        };
        assert_eq!(line.display(), "resume untrusted: nvim -S '\\u{202e}evil'");
        let parsed = parse(
            br#"{"abc":{"command":"sleep 30","trusted":true,"tried":false},"tok":{"command":"no"}}"#,
        );
        assert_eq!(parsed["abc"].command, "sleep 30");
        assert!(parsed["abc"].trusted);
        assert!(!parsed["tok"].trusted);
    }
}
