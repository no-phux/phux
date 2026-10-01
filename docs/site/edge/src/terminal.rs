//! Per-resource shell, retained VT replay, and replica generation state.

use bytes::Bytes;
use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::{ErrorCode, FrameKind};
use serde::{Deserialize, Serialize};

use crate::shell::{Shell, ShellCheckpoint};

pub(crate) const MAX_VIEWPORT: u16 = 1000;
const MAX_TRANSCRIPT_BYTES: usize = 8192;
const MAX_TRANSCRIPT_JSON_BYTES: usize = 16384;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalCheckpoint {
    pub id: u32,
    pub cols: u16,
    pub rows: u16,
    pub seq: u64,
    pub generation: u64,
    pub subscribed: bool,
    pub shell: ShellCheckpoint,
    pub transcript: String,
}

pub(crate) struct Terminal {
    pub id: u32,
    pub cols: u16,
    pub rows: u16,
    pub seq: u64,
    pub generation: u64,
    pub subscribed: bool,
    pub shell: Shell,
    transcript: String,
    transcript_json_bytes: usize,
}

impl Terminal {
    pub fn new(id: u32, cols: u16, rows: u16, shell: Shell) -> Self {
        let mut terminal = Self {
            id,
            cols,
            rows,
            seq: 0,
            generation: 0,
            subscribed: false,
            shell,
            transcript: String::new(),
            transcript_json_bytes: 0,
        };
        terminal.retain(&terminal.shell.greeting());
        terminal
    }

    pub fn resource_id(&self) -> ResourceId {
        ResourceId::new(self.id)
    }

    fn stream_id(&self) -> StreamId {
        StreamId::new(u64::from(self.id)).expect("validated nonzero resource id")
    }

    fn bootstrap_id(&self) -> BootstrapId {
        BootstrapId::new(self.generation).expect("subscribed terminal has a generation")
    }

    pub fn checkpoint(&self) -> TerminalCheckpoint {
        TerminalCheckpoint {
            id: self.id,
            cols: self.cols,
            rows: self.rows,
            seq: self.seq,
            generation: self.generation,
            subscribed: self.subscribed,
            shell: self.shell.checkpoint(),
            transcript: self.transcript.clone(),
        }
    }

    pub fn restore(
        checkpoint: TerminalCheckpoint,
        mode: &str,
        snapshot: &str,
    ) -> Result<Self, String> {
        if checkpoint.id == 0 || !valid_size(checkpoint.cols, checkpoint.rows) {
            return Err("checkpoint resource id or viewport is invalid".to_owned());
        }
        if checkpoint.seq == u64::MAX
            || checkpoint.generation == u64::MAX
            || (checkpoint.subscribed && checkpoint.generation == 0)
        {
            return Err("checkpoint sequence or generation is invalid".to_owned());
        }
        let transcript_json_bytes = json_string_bytes(&checkpoint.transcript);
        if checkpoint.transcript.len() > MAX_TRANSCRIPT_BYTES
            || transcript_json_bytes > MAX_TRANSCRIPT_JSON_BYTES
            || checkpoint.transcript.is_empty()
        {
            return Err("checkpoint transcript is invalid or too large".to_owned());
        }
        let mut shell = Shell::new(mode, snapshot);
        shell.restore(checkpoint.shell)?;
        Ok(Self {
            id: checkpoint.id,
            cols: checkpoint.cols,
            rows: checkpoint.rows,
            seq: checkpoint.seq,
            generation: checkpoint.generation,
            subscribed: checkpoint.subscribed,
            shell,
            transcript: checkpoint.transcript,
            transcript_json_bytes,
        })
    }

    /// Every replacement gets a fresh generation; the output sequence never resets.
    pub fn bootstrap(&mut self) -> Result<Vec<FrameKind>, String> {
        let generation = self
            .generation
            .checked_add(1)
            .filter(|value| *value < u64::MAX)
            .ok_or_else(|| "terminal generation exhausted; open a new terminal".to_owned())?;
        self.generation = generation;
        self.subscribed = true;
        let terminal_id = self.resource_id();
        let stream_id = self.stream_id();
        let bootstrap_id = self.bootstrap_id();
        // Replay at the new geometry rather than replacing the shell with its greeting.
        let payload = Bytes::from(format!("\x1b[0m\x1b[2J\x1b[H{}", self.transcript));
        Ok(vec![
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: self.cols,
                rows: self.rows,
                base_seq: self.seq,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload,
            },
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ])
    }

    pub fn output(&mut self, bytes: Vec<u8>) -> Vec<FrameKind> {
        if bytes.is_empty() {
            return Vec::new();
        }
        self.seq += 1; // Checked before applying input, so a refusal cannot mutate the shell.
        self.retain(&bytes);
        if !self.subscribed {
            return Vec::new();
        }
        vec![FrameKind::ResourceOutput {
            terminal_id: self.resource_id(),
            stream_id: self.stream_id(),
            bootstrap_id: self.bootstrap_id(),
            seq: self.seq,
            bytes: bytes.into(),
        }]
    }

    pub fn ensure_input_capacity(&self) -> Result<(), (ErrorCode, String)> {
        if self.seq >= u64::MAX - 1 {
            return Err((
                ErrorCode::ResourceExhausted,
                "terminal output sequence exhausted; open a new terminal".to_owned(),
            ));
        }
        Ok(())
    }

    fn retain(&mut self, bytes: &[u8]) {
        let text = std::str::from_utf8(bytes).expect("curated shell emits UTF-8 VT");
        // Portfolio navigation and clear-screen commands replace the visible screen.
        if let Some(offset) = text.rfind("\x1b[2J\x1b[H") {
            self.transcript.clear();
            self.transcript_json_bytes = 0;
            self.append_retained(&text[offset..]);
        } else {
            self.append_retained(text);
        }
        self.trim_retained();
    }

    fn trim_retained(&mut self) {
        // Evict complete lines only, and move the retained suffix just once.
        let mut start = 0;
        while self.transcript.len() - start > MAX_TRANSCRIPT_BYTES
            || self.transcript_json_bytes > MAX_TRANSCRIPT_JSON_BYTES
        {
            let Some(length) = self.transcript[start..]
                .find("\r\n")
                .map(|offset| offset + 2)
            else {
                self.compact_current_line();
                return;
            };
            self.transcript_json_bytes -=
                json_string_bytes(&self.transcript[start..start + length]);
            start += length;
        }
        self.transcript.drain(..start);
        if self.transcript.is_empty() {
            self.compact_current_line();
        }
    }

    fn compact_current_line(&mut self) {
        // Thousands of edits without Enter can exhaust replay while the logical
        // line remains small. Repaint that line, never discard the user's input.
        let current = String::from_utf8(self.shell.greeting()).expect("shell emits UTF-8");
        let encoded = json_string_bytes(&current);
        if current.len() <= MAX_TRANSCRIPT_BYTES && encoded <= MAX_TRANSCRIPT_JSON_BYTES {
            self.transcript = current;
            self.transcript_json_bytes = encoded;
        } else {
            // An oversized portfolio row cannot fit the bounded replay. Its
            // navigation state remains intact and the next key redraws it.
            self.transcript =
                "\x1b[0m\x1b[2J\x1b[HOutput exceeds the hosted replay limit.\r\n".to_owned();
            self.transcript_json_bytes = json_string_bytes(&self.transcript);
        }
    }

    fn append_retained(&mut self, text: &str) {
        self.transcript.push_str(text);
        self.transcript_json_bytes += json_string_bytes(text);
    }
}

pub(crate) fn valid_size(cols: u16, rows: u16) -> bool {
    (1..=MAX_VIEWPORT).contains(&cols) && (1..=MAX_VIEWPORT).contains(&rows)
}

// Excludes the surrounding quotes. Incrementally maintained, not rescanned per key.
fn json_string_bytes(text: &str) -> usize {
    text.bytes()
        .map(|byte| match byte {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 8 | 12 => 2,
            0..=31 => 6,
            _ => 1,
        })
        .sum()
}
