//! Standing producers (`[[producers]]`): any command the server keeps
//! running as a document producer (`docs/applications.md#standing-producers`).

use serde::{Deserialize, Serialize};

/// One command the server keeps running in its own session, named by
/// [`Self::session_name`]. The command runs in that session's pane, so it
/// opens its documents under the pane it finds in `PHUX_TERMINAL_ID`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProducerConfigEntry {
    /// The producer's name; its session is `producer-` and the name.
    pub name: String,
    /// The argv to run. A first word `phux` is the server's own binary.
    pub command: Vec<String>,
}

/// Every standing producer's session name starts with this; the server owns
/// such sessions and closes one whose entry is gone.
pub const PRODUCER_SESSION_PREFIX: &str = "producer-";

impl ProducerConfigEntry {
    /// The session the producer runs in: `producer-` and the name,
    /// lowercased, every run of other characters a `-`.
    #[must_use]
    pub fn session_name(&self) -> String {
        crate::mirror::session_name(PRODUCER_SESSION_PREFIX, &self.name)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_entry_parses_and_names_its_session() {
        let cfg: crate::Config = toml::from_str(
            "[[producers]]\nname = \"My Downloads\"\n\
             command = [\"phux\", \"agent\", \"doc\", \"folder\", \"/tmp\"]\n",
        )
        .expect("parse");
        let [entry] = cfg.producers.as_slice() else {
            panic!("one entry");
        };
        assert_eq!(entry.session_name(), "producer-my-downloads");
        assert_eq!(entry.command[0], "phux");
        assert!(toml::from_str::<crate::Config>("[[producers]]\nname = \"x\"\n").is_err());
    }
}
