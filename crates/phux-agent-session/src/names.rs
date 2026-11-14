//! Server-assigned agent names (`docs/agents.md#driving-an-agent-in-a-pane`).
//!
//! The detector, the launch wrapper and every integration write a record
//! whose `name` is its `kind` (`opencode`). Stored as written, two OpenCode
//! panes would share one name and `%opencode` would reach neither. So every
//! write of such a record gets a name no other pane's record carries: the
//! kind itself for the first pane, then `opencode-2`, `opencode-3`, ... A
//! pane keeps the name it holds for as long as its record does, so
//! `%opencode` keeps reaching the first pane while it runs. A name a writer
//! chose (`reviewer`) is never changed.

use std::collections::HashSet;

use crate::agent_record::AgentRecordJson;

/// `value`, a `phux.agent/v1` record about to be stored on a pane whose
/// current record is `held`, with a name no record in `others` carries when
/// its name is its kind. Any other value, and a record whose kind name is
/// free, comes back byte for byte.
///
/// ponytail: names live in the records, which a graceful upgrade leaves for
/// their writers to republish, so two panes of one kind can trade names
/// across an upgrade; carry the names in the upgrade blob if that matters.
#[must_use]
pub fn with_unique_name<'a>(
    value: Vec<u8>,
    held: Option<&[u8]>,
    others: impl Iterator<Item = &'a [u8]>,
) -> Vec<u8> {
    let Some(mut record) = AgentRecordJson::decode(&value) else {
        return value;
    };
    if !record
        .kind
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case(&record.name))
    {
        return value;
    }
    let taken: HashSet<String> = others
        .filter_map(AgentRecordJson::decode)
        .map(|other| other.name.to_ascii_lowercase())
        .collect();
    let free = |name: &str| !taken.contains(&name.to_ascii_lowercase());
    let base = record.name.clone();
    let held = held
        .and_then(AgentRecordJson::decode)
        .map(|current| current.name)
        .filter(|name| is_kind_name(name, &base) && free(name));
    let name = held.unwrap_or_else(|| {
        std::iter::once(base.clone())
            .chain((2_u32..).map(|n| format!("{base}-{n}")))
            .find(|candidate| free(candidate))
            .unwrap_or_else(|| base.clone())
    });
    if name == base {
        return value;
    }
    record.name = name;
    record.encode()
}

/// Whether `name` is one [`with_unique_name`] hands out for `base`: `base`
/// itself or `base-N`.
fn is_kind_name(name: &str, base: &str) -> bool {
    let Some(head) = name
        .get(..base.len())
        .filter(|head| head.eq_ignore_ascii_case(base))
    else {
        return false;
    };
    let suffix = &name[head.len()..];
    suffix.is_empty()
        || suffix
            .strip_prefix('-')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use std::collections::BTreeMap;

    use super::{is_kind_name, with_unique_name};

    /// Panes and their stored records, written the way the server stores them.
    #[derive(Default)]
    struct Store(BTreeMap<u32, Vec<u8>>);

    impl Store {
        fn write(&mut self, pane: u32, value: &str) -> String {
            let stored = with_unique_name(
                value.as_bytes().to_vec(),
                self.0.get(&pane).map(Vec::as_slice),
                self.0
                    .iter()
                    .filter(|(id, _)| **id != pane)
                    .map(|(_, v)| v.as_slice()),
            );
            self.0.insert(pane, stored.clone());
            let record: serde_json::Value = serde_json::from_slice(&stored).expect("json");
            record["name"].as_str().expect("name").to_owned()
        }
    }

    const OPENCODE: &str = r#"{"name":"opencode","kind":"opencode","state":"idle"}"#;

    #[test]
    fn each_pane_of_a_kind_gets_its_own_stable_name() {
        let mut store = Store::default();
        assert_eq!(store.write(1, OPENCODE), "opencode");
        assert_eq!(store.write(2, OPENCODE), "opencode-2");
        assert_eq!(store.write(3, OPENCODE), "opencode-3");
        // Every later write keeps each pane's name.
        assert_eq!(store.write(2, OPENCODE), "opencode-2");
        assert_eq!(store.write(1, OPENCODE), "opencode");
        // A freed name goes to the next new pane; held names never move.
        store.0.remove(&1);
        assert_eq!(store.write(3, OPENCODE), "opencode-3");
        assert_eq!(store.write(4, OPENCODE), "opencode");
    }

    #[test]
    fn a_chosen_name_and_a_free_kind_name_are_stored_as_written() {
        let mut store = Store::default();
        let chosen = r#"{"name":"reviewer","kind":"opencode","state":"idle"}"#;
        let unchanged = |store: &Store, value: &str| {
            with_unique_name(
                value.as_bytes().to_vec(),
                None,
                store.0.values().map(Vec::as_slice),
            ) == value.as_bytes()
        };
        assert!(unchanged(&store, OPENCODE));
        assert_eq!(store.write(1, chosen), "reviewer");
        assert_eq!(
            store.write(2, chosen),
            "reviewer",
            "a chosen clash stays ambiguous"
        );
        assert!(unchanged(&store, OPENCODE));
        assert!(unchanged(&store, r#"{"name":"opencode","state":"idle"}"#));
        assert!(unchanged(&store, "not json"));
    }

    #[test]
    fn panes_that_share_a_kind_name_are_told_apart_on_their_next_write() {
        // An older server stored the kind name on both panes.
        let mut store = Store::default();
        store.0.insert(1, OPENCODE.as_bytes().to_vec());
        store.0.insert(2, OPENCODE.as_bytes().to_vec());
        assert_eq!(store.write(1, OPENCODE), "opencode-2");
        assert_eq!(store.write(2, OPENCODE), "opencode");
    }

    #[test]
    fn kind_names_are_the_kind_or_kind_dash_number() {
        assert!(is_kind_name("opencode", "opencode"));
        assert!(is_kind_name("opencode-12", "opencode"));
        assert!(!is_kind_name("opencode-", "opencode"));
        assert!(!is_kind_name("opencode-x", "opencode"));
        assert!(!is_kind_name("reviewer", "opencode"));
        assert!(!is_kind_name("open", "opencode"));
    }
}
