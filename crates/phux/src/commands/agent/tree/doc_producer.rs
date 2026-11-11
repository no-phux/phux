//! The producer loop `phux agent doc serve|folder|notifications` share:
//! open a typed document under a pane, publish whole documents, receive the
//! input intents viewers send, and re-adopt the document when a server
//! upgrade drops the connection (`docs/applications.md#publish-your-own-kind`).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_client::tree::{self, DocumentInput, TreeError};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::FrameKind;
use phux_server::runtime::default_socket_path;
use serde_json::{Value, json};

use super::{SCHEMA_VERSION, dial, mirror_pane, redial, report};

/// One typed document this process produces.
pub(super) struct Producer {
    conn: Connection,
    socket: PathBuf,
    resource: ResourceId,
    /// The newest document, published again after a redial.
    last: Option<Vec<u8>>,
    queued: VecDeque<DocumentInput>,
}

#[allow(clippy::future_not_send, reason = "the CLI runtime is single-threaded")]
impl Producer {
    /// Open a document of `content_type` under `pane` (this process's own
    /// pane when `None`) and print its JSON line on stdout.
    pub(super) async fn open(
        socket: Option<PathBuf>,
        pane: Option<&str>,
        content_type: &str,
        name: Option<&str>,
        verb: &str,
    ) -> Result<Self, ExitCode> {
        let socket = socket.unwrap_or_else(default_socket_path);
        let parent = mirror_pane(&socket, pane, verb, false).await?;
        let mut conn = dial(&socket, false).await?;
        let opened = tree::open_document(&mut conn, &parent, content_type, name)
            .await
            .map_err(|err| report(false, &err))?;
        outln!(
            "{}",
            json!({
                "schema_version": SCHEMA_VERSION,
                "event": "opened",
                "resource": phux_client::selector::format_terminal_id(&opened.resource),
                "parent": phux_client::selector::format_terminal_id(&opened.parent),
                "content_type": content_type,
                "name": name,
            })
        );
        Ok(Self {
            conn,
            socket,
            resource: opened.resource,
            last: None,
            queued: VecDeque::new(),
        })
    }

    /// Replace the document, redialing (and so re-adopting it) when the
    /// connection is lost.
    pub(super) async fn publish(&mut self, bytes: Vec<u8>) -> Result<(), TreeError> {
        let mut intents = Vec::new();
        let sent = redial::publish(
            &mut self.conn,
            &self.socket,
            &self.resource,
            &bytes,
            &mut intents,
        )
        .await;
        self.queue(intents);
        sent?;
        self.last = Some(bytes);
        Ok(())
    }

    /// Wait for the next frame; cancel-safe, so it can sit in a
    /// `tokio::select!`. Hand what it returns to [`Self::settle`].
    pub(super) async fn recv(&mut self) -> Result<Option<FrameKind>, AttachError> {
        if !self.queued.is_empty() {
            return Ok(None);
        }
        self.conn.recv().await.map(Some)
    }

    /// The next intent, after acting on what [`Self::recv`] woke for: a
    /// lost connection is redialed and the newest document published again.
    /// A clipboard copy is refused here; a typed document has no clipboard.
    pub(super) async fn settle(
        &mut self,
        woke: Result<Option<FrameKind>, AttachError>,
    ) -> Result<Option<DocumentInput>, TreeError> {
        match woke {
            Ok(frame) => self.queue(frame.as_ref().and_then(tree::intent)),
            Err(err) if err.is_transport_loss() => {
                self.conn = redial::redial(&self.socket).await.ok_or(err)?;
                if let Some(last) = self.last.take() {
                    self.publish(last).await?;
                }
            }
            Err(err) => return Err(err.into()),
        }
        match self.queued.pop_front() {
            Some(DocumentInput::CopyClipboard { reply }) => {
                let refusal = Err("this document has no clipboard".to_owned());
                let mut intents = Vec::new();
                let sent =
                    tree::answer_copy(&mut self.conn, &self.resource, reply, refusal, &mut intents)
                        .await;
                self.queue(intents);
                sent.map(|()| None)
            }
            next => Ok(next),
        }
    }

    fn queue(&mut self, intents: impl IntoIterator<Item = tree::Intent>) {
        let mine = intents.into_iter().filter(|i| i.resource == self.resource);
        self.queued.extend(mine.map(|i| i.input));
    }
}

/// `input` as the JSON line `phux agent doc serve` prints, or `None` for
/// the clipboard answers a producer never receives.
pub(super) fn intent_json(input: &DocumentInput) -> Option<Value> {
    let (intent, fields) = match input {
        DocumentInput::Navigate { url } => ("navigate", json!({ "url": url })),
        DocumentInput::Click { x, y } => ("click", json!({ "x": x, "y": y })),
        DocumentInput::Type { text } => ("type", json!({ "text": text })),
        DocumentInput::Key { key } => ("key", json!({ "key": key.name() })),
        DocumentInput::Scroll { dx, dy } => ("scroll", json!({ "dx": dx, "dy": dy })),
        DocumentInput::Paste { text, chord } => ("paste", json!({ "text": text, "chord": chord })),
        DocumentInput::Activate { node } => ("activate", json!({ "node": node })),
        DocumentInput::Drag { from, to } => (
            "drag",
            json!({ "from": [from.0, from.1], "to": [to.0, to.1] }),
        ),
        DocumentInput::CopyClipboard { .. } | DocumentInput::ClipboardText { .. } => return None,
    };
    let mut line = json!({ "schema_version": SCHEMA_VERSION, "event": "input", "intent": intent });
    if let (Some(line), Value::Object(fields)) = (line.as_object_mut(), fields) {
        line.extend(fields);
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use phux_client::tree::DocumentKey;

    use super::*;

    #[test]
    fn intents_print_as_flat_json_lines() {
        let line = intent_json(&DocumentInput::Click { x: 3, y: 4 }).expect("click");
        assert_eq!(line["event"], "input");
        assert_eq!(line["intent"], "click");
        assert_eq!((line["x"].as_u64(), line["y"].as_u64()), (Some(3), Some(4)));
        let key = intent_json(&DocumentInput::Key {
            key: DocumentKey::Enter,
        });
        assert_eq!(key.expect("key")["key"], "Enter");
        assert!(intent_json(&DocumentInput::CopyClipboard { reply: 1 }).is_none());
    }
}
