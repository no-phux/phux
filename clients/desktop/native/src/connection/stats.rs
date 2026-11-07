//! `phux.agent-stats/v1`: a host's tally of work done per pane, published by
//! the opt-in agent-stats plugin (plugins/agent-stats/README.md#contract).
//! Usually absent. Absent, unreadable or another version reads as no tally;
//! it never errors.

use std::collections::HashMap;

use phux_protocol::ResourceId;

/// The Global metadata key each host publishes its tally under.
pub(crate) const KEY: &str = "phux.agent-stats/v1";

/// One pane's tally.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentStats {
    /// Tasks finished.
    pub turns: u32,
    /// Seconds spent working, up to when the host wrote the document.
    pub working_s: u64,
    /// When this client received it (Unix ms); zero keeps no live clock.
    pub at_ms: u64,
}

impl AgentStats {
    /// Seconds working by `now_ms`: a working agent's open stretch goes on
    /// counting after the document was written, as the contract says.
    pub fn working_now(&self, working: bool, now_ms: u64) -> u64 {
        let open = if working && self.at_ms > 0 {
            now_ms.saturating_sub(self.at_ms) / 1000
        } else {
            0
        };
        self.working_s + open
    }
}

#[derive(serde::Deserialize)]
struct Document {
    v: u32,
    agents: HashMap<String, Entry>,
}

#[derive(serde::Deserialize)]
struct Entry {
    #[serde(default)]
    turns: u32,
    #[serde(default)]
    working_s: u64,
}

/// The document's tallies by terminal number, received at `at_ms`; empty
/// when the value is absent, unparseable or not version 1.
pub(crate) fn parse(value: Option<&[u8]>, at_ms: u64) -> HashMap<u32, AgentStats> {
    let Some(document) = value.and_then(|bytes| serde_json::from_slice::<Document>(bytes).ok())
    else {
        return HashMap::new();
    };
    if document.v != 1 {
        return HashMap::new();
    }
    document
        .agents
        .into_iter()
        .filter_map(|(id, entry)| {
            let id = id.trim().trim_start_matches('@').parse().ok()?;
            let stats = AgentStats {
                turns: entry.turns,
                working_s: entry.working_s,
                at_ms,
            };
            Some((id, stats))
        })
        .collect()
}

/// The tally for `terminal`, matched by its number on its own host.
pub(crate) fn of(tallies: &HashMap<u32, AgentStats>, terminal: &ResourceId) -> Option<AgentStats> {
    tallies.get(&terminal.local_id()?).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_maps_by_terminal_number() {
        let doc = br#"{"v":1,"agents":{"12":{"kind":"claude","name":"fox","turns":2,"working_s":76,"first_seen_ms":1,"updated_ms":2,"later":true},"@3":{"turns":40}}}"#;
        let tallies = parse(Some(doc), 9);
        let twelve = of(&tallies, &ResourceId::local(12)).expect("matched");
        assert_eq!((twelve.turns, twelve.working_s, twelve.at_ms), (2, 76, 9));
        assert_eq!(
            of(&tallies, &ResourceId::local(3)).map(|s| s.turns),
            Some(40)
        );
        assert_eq!(of(&tallies, &ResourceId::local(4)), None, "not in the map");
    }

    #[test]
    fn absent_garbage_and_other_versions_read_as_nothing() {
        for value in [
            None,
            Some(&b"not json"[..]),
            Some(br#"{"v":2,"agents":{"1":{"turns":3}}}"#),
            Some(br#"{"agents":{"1":{"turns":3}}}"#),
            Some(br#"{"v":1,"agents":{"1":{"turns":-3}}}"#),
            Some(br#"{"v":1,"agents":{"x":{"turns":3}}}"#),
        ] {
            assert!(parse(value, 1).is_empty(), "{value:?}");
        }
    }

    #[test]
    fn a_working_agent_keeps_counting_after_the_write() {
        let stats = AgentStats {
            turns: 0,
            working_s: 60,
            at_ms: 1_000,
        };
        assert_eq!(stats.working_now(true, 31_000), 90);
        assert_eq!(stats.working_now(false, 31_000), 60);
        let fixture = AgentStats { at_ms: 0, ..stats };
        assert_eq!(fixture.working_now(true, 31_000), 60);
    }
}
