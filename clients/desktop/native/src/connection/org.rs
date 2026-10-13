//! Organization reads and outcomes as the window shows them: arrangement
//! group names per terminal and operation outcomes.

use std::collections::HashMap;

use phux_client_core::layout::leaves;
use phux_client_core::org_ops::{OrgOutcome, refusal_sentence};
use phux_client_runtime::control::OrganizationReplica;
use phux_protocol::ResourceId;

/// The arrangement group that shows each terminal, by name, for every
/// session whose arrangement the replica holds.
#[must_use]
pub fn group_names(replica: &OrganizationReplica) -> HashMap<ResourceId, String> {
    let mut names = HashMap::new();
    for session in replica.sessions() {
        let Some((_, _, document, _)) = replica.arrangement_document(&session.key) else {
            continue;
        };
        for group in &document.groups {
            let Some(tree) = group.state.tree.as_ref() else {
                continue;
            };
            for terminal in leaves(tree) {
                names.entry(terminal).or_insert_with(|| group.name.clone());
            }
        }
    }
    names
}

/// A sentence for an organization operation that did not take effect.
#[must_use]
pub fn outcome_sentence(outcome: &OrgOutcome) -> String {
    match outcome {
        OrgOutcome::Refused(refusal) => refusal_sentence(refusal),
        OrgOutcome::Error { message, .. } => message.clone(),
        OrgOutcome::Unknown { reason } => {
            format!("the outcome is unknown ({reason}); check the sidebar before retrying")
        }
        OrgOutcome::NotSent { reason } => format!("not sent: {reason}"),
        _ => "it took effect".to_owned(),
    }
}
