//! The persisted shape of a native agent's controller state
//! ([`StateBlob::native_agents`](super::StateBlob::native_agents)). The
//! `AgentSession` app owns what it means; these field names are the format.

use std::collections::{BTreeMap, VecDeque};

use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::{
    NativeCommand, NativeCommandResult, NativePublicationReceipt, NativeView, SpawnNativeAgent,
};
use serde::{Deserialize, Serialize};

/// One reserved controller operation and its receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeOperation {
    /// The command as its requester sent it.
    pub command: NativeCommand,
    /// Its latest receipt.
    pub result: NativeCommandResult,
    /// Whether it was dispatched to the host.
    pub delivered: bool,
    /// Sticky across ambiguous receipts and upgrade: submission cannot be undone.
    #[serde(default = "assume_submitted")]
    pub submitted: bool,
    /// The requesting connection.
    pub requester: u64,
}

const fn assume_submitted() -> bool {
    true
}

/// One view publication and its stream receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativePublication {
    /// The publication id, increasing per binding.
    pub id: u64,
    /// The published content's fingerprint.
    pub fingerprint: String,
    /// The stream's acceptance, once known.
    pub receipt: Option<NativePublicationReceipt>,
    /// The view it published, until acknowledged.
    pub view: Option<NativeView>,
}

/// Private upgrade state; never exported as organization launch intent.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeBinding {
    /// The recipe the guardian launched.
    pub launch: SpawnNativeAgent,
    /// The agent session it feeds.
    pub agent: ResourceId,
    /// Its guardian.
    pub parent: ResourceId,
    /// The guardian activation it is fenced to.
    pub activation: String,
    /// The proof its host presents.
    pub proof: String,
    /// The credential its host authenticates with.
    pub credential: String,
    /// The host's process id, once it connected.
    pub host_pid: Option<u32>,
    /// Raised on every upgrade so a stale host is refused.
    pub generation: u64,
    /// The newest accepted view.
    pub view: NativeView,
    /// Whether the controller closed.
    pub closed: bool,
    /// Whether its closure reached the stream.
    #[serde(default)]
    pub closure_committed: bool,
    /// The newest publication whose identity is durably committed.
    #[serde(default)]
    pub publication_committed: u64,
    /// The newest evidence sequence applied.
    #[serde(default)]
    pub evidence_seq: u64,
    /// Every reserved operation, by id.
    pub operations: BTreeMap<u64, NativeOperation>,
    /// Operations waiting for the host, in order.
    pub pending: VecDeque<u64>,
    /// Each answered question's reserving operation.
    pub answered: BTreeMap<String, u64>,
    /// Publications not yet acknowledged.
    pub publications: VecDeque<NativePublication>,
    /// The newest publication id issued.
    pub publication_high_water: u64,
}

impl std::fmt::Debug for NativeBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeBinding")
            .field("agent", &self.agent)
            .field("parent", &self.parent)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}
