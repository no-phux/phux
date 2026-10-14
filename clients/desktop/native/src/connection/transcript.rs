//! Bounded native projection of committed agent publications, never a second session.

use std::collections::VecDeque;

use phux_client_core::session::AgentStreamInfo;
use phux_client_core::session::agent_stream::{
    AgentEventKind, AgentEventRecord, AgentPublication, AgentRecordMode, AgentSessionState,
    AgentSessionStatus,
};
use phux_client_core::session::agent_transcript::{
    TranscriptEntry, TranscriptRole, clean_text, keep_head,
};
use phux_client_runtime::control::AgentSessionInfo;
use phux_protocol::ResourceId;

const MAX_ENTRIES: usize = 256;
const MAX_BYTES: usize = 2 * 1024 * 1024;

pub struct Conversation {
    pub resource: ResourceId,
    pub parent: ResourceId,
    pub epoch: u64,
    pub stream: AgentStreamInfo,
    pub state: AgentSessionState,
    pub entries: VecDeque<TranscriptEntry>,
    pub closed: bool,
    pub revision: u64,
    pub omitted: bool,
    pub last_activity_ms: u64,
    pub stop_requested: bool,
    has_transcript: bool,
    bytes: usize,
}

impl Conversation {
    pub fn new(
        publication: &AgentPublication,
        info: &AgentSessionInfo,
        epoch: u64,
    ) -> Option<Self> {
        Some(Self {
            resource: publication.resource_id.clone(),
            parent: info.parent.clone()?,
            epoch,
            stream: publication.stream,
            state: AgentSessionState {
                provider: info.provider.clone(),
                native_id: info.native_id.clone(),
                status: AgentSessionStatus::Unknown,
            },
            entries: VecDeque::new(),
            closed: false,
            revision: 0,
            omitted: false,
            last_activity_ms: 0,
            stop_requested: false,
            has_transcript: false,
            bytes: 0,
        })
    }

    pub fn apply(&mut self, publication: &AgentPublication, info: &AgentSessionInfo, epoch: u64) {
        if self.closed || info.parent.as_ref() != Some(&self.parent) {
            return;
        }
        if publication.mode == AgentRecordMode::Replace {
            self.entries.clear();
            self.bytes = 0;
            self.omitted = false;
            self.has_transcript = false;
            self.state = AgentSessionState {
                provider: info.provider.clone(),
                native_id: info.native_id.clone(),
                status: AgentSessionStatus::Unknown,
            };
        } else if publication.stream.stream_id == self.stream.stream_id
            && publication.stream.last_seq < self.stream.last_seq
        {
            return;
        }
        for record in &publication.records {
            // The runtime has already fenced publication generations and replay cuts.
            self.state.fold(record);
            if record.seq > self.stream.last_seq && self.state.status != AgentSessionStatus::Working
            {
                self.stop_requested = false;
            }
            self.last_activity_ms = self.last_activity_ms.max(record.ts_ms);
            if let Some(entry) = record.transcript_entry() {
                if !self.has_transcript {
                    self.entries.clear();
                    self.bytes = 0;
                    self.omitted = false;
                    self.has_transcript = true;
                }
                self.upsert(entry);
            } else if !self.has_transcript
                && let Some(entry) = timeline(record)
            {
                self.upsert(entry);
            }
        }
        if self.state.status != AgentSessionStatus::Working {
            self.stop_requested = false;
        }
        self.epoch = epoch;
        self.stream = publication.stream;
        self.revision = self.revision.saturating_add(1);
    }

    fn upsert(&mut self, mut entry: TranscriptEntry) {
        entry.text = clean_text(&entry.text);
        keep_head(&mut entry.text, 16 * 1024);
        if let Some(tool) = entry.tool.as_mut() {
            tool.name = clean_text(&tool.name);
            tool.summary = clean_text(&tool.summary);
            tool.output = clean_text(&tool.output);
        }
        let bytes = entry_bytes(&entry);
        if let Some(previous) = self.entries.iter_mut().find(|row| row.id == entry.id) {
            self.bytes = self.bytes.saturating_sub(entry_bytes(previous)) + bytes;
            *previous = entry;
        } else {
            self.bytes += bytes;
            self.entries.push_back(entry);
        }
        while self.entries.len() > MAX_ENTRIES || self.bytes > MAX_BYTES {
            if let Some(entry) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(entry_bytes(&entry));
                self.omitted = true;
            }
        }
    }
}

fn entry_bytes(entry: &TranscriptEntry) -> usize {
    entry.id.len()
        + entry.text.len()
        + entry.tool.as_ref().map_or(0, |tool| {
            tool.name.len() + tool.call_id.len() + tool.summary.len() + tool.output.len()
        })
}

fn timeline(record: &AgentEventRecord) -> Option<TranscriptEntry> {
    let text = match &record.kind {
        AgentEventKind::SessionStart => "Session started".to_owned(),
        AgentEventKind::Prompt => "Prompt submitted (text not shared)".to_owned(),
        AgentEventKind::ToolStart => format!(
            "Tool started: {}",
            record.data_str("tool_name").unwrap_or("tool")
        ),
        AgentEventKind::ToolEnd => "Tool finished".to_owned(),
        AgentEventKind::Ask => record
            .data_str("question")
            .unwrap_or("Agent needs your answer")
            .to_owned(),
        AgentEventKind::Notification => record
            .data_str("message")
            .unwrap_or("Agent notification")
            .to_owned(),
        AgentEventKind::Stop => "Turn finished".to_owned(),
        AgentEventKind::SessionEnd => "Session ended".to_owned(),
        AgentEventKind::State => {
            format!("State: {}", record.data_str("state").unwrap_or("unknown"))
        }
        AgentEventKind::ProviderRaw | AgentEventKind::Unknown(_) => return None,
    };
    Some(TranscriptEntry {
        id: format!("timeline:{}", record.seq),
        role: TranscriptRole::System,
        text,
        truncated: false,
        is_final: true,
        tool: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ids::{BootstrapId, StreamId};
    use serde_json::json;

    fn publication(
        mode: AgentRecordMode,
        seq: u64,
        records: Vec<AgentEventRecord>,
    ) -> AgentPublication {
        AgentPublication {
            resource_id: ResourceId::local(2),
            stream: AgentStreamInfo {
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                last_seq: seq,
            },
            mode,
            records,
        }
    }

    fn row(seq: u64, id: &str, text: &str) -> AgentEventRecord {
        AgentEventRecord {
            seq,
            ts_ms: 0,
            kind: AgentEventKind::ProviderRaw,
            data: json!({
                "provider": "omp", "schema": "phux.transcript/v1", "entry": {
                    "id": id, "role": "assistant", "text": text, "truncated": false, "final": false
                }
            }),
        }
    }

    #[test]
    fn stable_rows_replace_and_replay_replaces_the_log() {
        let info = AgentSessionInfo {
            parent: Some(ResourceId::local(1)),
            provider: Some("omp".into()),
            ..Default::default()
        };
        let first = publication(AgentRecordMode::Replace, 1, vec![row(1, "reply", "a")]);
        let mut conversation = Conversation::new(&first, &info, 1).unwrap();
        conversation.apply(&first, &info, 1);
        conversation.apply(
            &publication(AgentRecordMode::Append, 2, vec![row(2, "reply", "abc")]),
            &info,
            1,
        );
        assert_eq!(conversation.entries.len(), 1);
        assert_eq!(conversation.entries[0].text, "abc");
        conversation.apply(
            &publication(AgentRecordMode::Replace, 3, Vec::new()),
            &info,
            2,
        );
        assert!(conversation.entries.is_empty());
        assert_eq!(conversation.epoch, 2);
    }

    #[test]
    fn closed_or_reparented_stream_cannot_accept_rows() {
        let mut info = AgentSessionInfo {
            parent: Some(ResourceId::local(1)),
            ..Default::default()
        };
        let batch = publication(AgentRecordMode::Replace, 1, vec![row(1, "reply", "a")]);
        let mut conversation = Conversation::new(&batch, &info, 1).unwrap();
        info.parent = Some(ResourceId::local(3));
        conversation.apply(&batch, &info, 1);
        assert!(conversation.entries.is_empty());
        info.parent = Some(ResourceId::local(1));
        conversation.closed = true;
        conversation.apply(&batch, &info, 1);
        assert!(conversation.entries.is_empty());
    }

    #[test]
    fn retention_is_bounded_and_lifecycle_only_is_readable() {
        let info = AgentSessionInfo {
            parent: Some(ResourceId::local(1)),
            ..Default::default()
        };
        let batch = publication(
            AgentRecordMode::Replace,
            300,
            (1..=300)
                .map(|seq| AgentEventRecord {
                    seq,
                    ts_ms: 0,
                    kind: AgentEventKind::Prompt,
                    data: json!({"length": 20}),
                })
                .collect(),
        );
        let mut conversation = Conversation::new(&batch, &info, 1).unwrap();
        conversation.apply(&batch, &info, 1);
        assert_eq!(conversation.entries.len(), MAX_ENTRIES);
        assert!(conversation.omitted);
        assert_eq!(conversation.state.status, AgentSessionStatus::Working);
        assert_eq!(
            conversation.entries[0].text,
            "Prompt submitted (text not shared)"
        );
    }

    #[test]
    fn permission_and_stop_state_is_folded_between_working_episodes() {
        let info = AgentSessionInfo {
            parent: Some(ResourceId::local(1)),
            ..Default::default()
        };
        let batch = publication(
            AgentRecordMode::Replace,
            1,
            vec![AgentEventRecord {
                seq: 1,
                ts_ms: 0,
                kind: AgentEventKind::Prompt,
                data: json!({}),
            }],
        );
        let mut conversation = Conversation::new(&batch, &info, 1).unwrap();
        conversation.apply(&batch, &info, 1);
        conversation.stop_requested = true;
        conversation.apply(
            &publication(
                AgentRecordMode::Append,
                2,
                vec![row(2, "reply", "streaming")],
            ),
            &info,
            1,
        );
        assert!(
            conversation.stop_requested,
            "text deltas do not re-arm Stop"
        );
        conversation.apply(
            &publication(
                AgentRecordMode::Append,
                3,
                vec![AgentEventRecord {
                    seq: 3,
                    ts_ms: 0,
                    kind: AgentEventKind::Notification,
                    data: json!({"kind":"permission"}),
                }],
            ),
            &info,
            1,
        );
        assert_eq!(conversation.state.status, AgentSessionStatus::Blocked);
        assert!(!conversation.stop_requested);
        conversation.apply(
            &publication(
                AgentRecordMode::Append,
                4,
                vec![AgentEventRecord {
                    seq: 4,
                    ts_ms: 0,
                    kind: AgentEventKind::Prompt,
                    data: json!({}),
                }],
            ),
            &info,
            1,
        );
        assert_eq!(conversation.state.status, AgentSessionStatus::Working);
        assert!(!conversation.stop_requested);
    }

    #[test]
    fn transcript_supersedes_lifecycle_fallback_without_losing_state() {
        let info = AgentSessionInfo {
            parent: Some(ResourceId::local(1)),
            ..Default::default()
        };
        let prompt = AgentEventRecord {
            seq: 1,
            ts_ms: 0,
            kind: AgentEventKind::Prompt,
            data: json!({}),
        };
        let batch = publication(AgentRecordMode::Replace, 1, vec![prompt.clone()]);
        let mut conversation = Conversation::new(&batch, &info, 1).unwrap();
        conversation.apply(&batch, &info, 1);
        assert_eq!(conversation.entries[0].role, TranscriptRole::System);
        conversation.apply(
            &publication(
                AgentRecordMode::Append,
                2,
                vec![row(2, "reply", "actual reply")],
            ),
            &info,
            1,
        );
        assert_eq!(conversation.entries.len(), 1);
        assert_eq!(conversation.entries[0].id, "reply");
        conversation.apply(
            &publication(
                AgentRecordMode::Append,
                3,
                vec![AgentEventRecord {
                    seq: 3,
                    ts_ms: 0,
                    kind: AgentEventKind::Stop,
                    data: json!({}),
                }],
            ),
            &info,
            1,
        );
        assert_eq!(conversation.entries.len(), 1);
        assert_eq!(conversation.state.status, AgentSessionStatus::Done);
        conversation.apply(
            &publication(AgentRecordMode::Replace, 4, vec![prompt]),
            &info,
            2,
        );
        assert_eq!(conversation.entries[0].role, TranscriptRole::System);
    }

    #[test]
    fn retained_replay_does_not_rearm_stop_for_the_same_working_episode() {
        let info = AgentSessionInfo {
            parent: Some(ResourceId::local(1)),
            ..Default::default()
        };
        let records = [
            AgentEventKind::Prompt,
            AgentEventKind::Stop,
            AgentEventKind::Prompt,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, kind)| AgentEventRecord {
            seq: index as u64 + 1,
            ts_ms: 0,
            kind,
            data: json!({}),
        })
        .collect();
        let batch = publication(AgentRecordMode::Replace, 3, records);
        let mut conversation = Conversation::new(&batch, &info, 1).unwrap();
        conversation.apply(&batch, &info, 1);
        conversation.stop_requested = true;
        conversation.apply(&batch, &info, 2);
        assert_eq!(conversation.state.status, AgentSessionStatus::Working);
        assert!(
            conversation.stop_requested,
            "historical Stop does not re-arm current Stop"
        );
        conversation.apply(
            &publication(
                AgentRecordMode::Append,
                4,
                vec![AgentEventRecord {
                    seq: 4,
                    ts_ms: 0,
                    kind: AgentEventKind::Stop,
                    data: json!({}),
                }],
            ),
            &info,
            2,
        );
        assert!(!conversation.stop_requested);
    }
}
