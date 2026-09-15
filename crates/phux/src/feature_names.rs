//! One `snake_case` name per `ServerFeature` bit, shared by
//! `phux status --json` (the negotiated `features` list) and
//! `phux --capabilities --json` (the kind catalog's gates), so both name a
//! bit the same way.
//!
//! Each name is its `docs/spec/proto.md` §6.2 constant lower-cased; a test
//! reads the spec to hold that, and checks that every bit this build knows
//! has a name.

use phux_protocol::caps::{ServerFeature, ServerFeatureSet};

/// Every named feature, in bit order.
const NAMES: &[(ServerFeature, &str)] = &[
    (ServerFeature::AcknowledgedInput, "acknowledged_input"),
    (ServerFeature::FileUpload, "file_upload"),
    (ServerFeature::MoveResource, "move_resource"),
    (ServerFeature::TerminalReply, "terminal_reply"),
    (ServerFeature::Shutdown, "shutdown"),
    (ServerFeature::SpawnInitialSize, "spawn_initial_size"),
    (ServerFeature::ReportAgentState, "report_agent_state"),
    (ServerFeature::GetPerf, "get_perf"),
    (ServerFeature::Transcribe, "transcribe"),
    (ServerFeature::ResourceKinds, "resource_kinds"),
    (ServerFeature::ListDirectory, "list_directory"),
    (ServerFeature::HostSessions, "host_sessions"),
    (ServerFeature::KeepEmptySessions, "keep_empty_sessions"),
    (ServerFeature::Whoami, "whoami"),
    (ServerFeature::ListDirectoryHost, "list_directory_host"),
    (ServerFeature::SshOrigin, "ssh_origin"),
    (ServerFeature::ConditionalKill, "conditional_kill"),
    (ServerFeature::QuicStreams, "quic_streams"),
    (ServerFeature::OpenListener, "open_listener"),
    (ServerFeature::EventJournal, "event_journal"),
    (ServerFeature::RetainOnExit, "retain_on_exit"),
    (ServerFeature::SpawnIdempotency, "spawn_idempotency"),
    (ServerFeature::AttachRoles, "attach_roles"),
];

/// The name of `feature`. `None` only for a bit missing from the table,
/// which the tests rule out for every bit this build knows.
pub(crate) fn feature_name(feature: ServerFeature) -> Option<&'static str> {
    NAMES
        .iter()
        .find(|(named, _)| *named == feature)
        .map(|(_, name)| *name)
}

/// Every feature in `features` by name, in bit order. A bit this binary
/// does not know is not named.
pub(crate) fn feature_names(features: ServerFeatureSet) -> Vec<&'static str> {
    NAMES
        .iter()
        .filter(|(feature, _)| features.contains(*feature))
        .map(|(_, name)| *name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{NAMES, feature_names};
    use phux_protocol::caps::{ServerFeature, ServerFeatureSet};

    /// Whether `line` declares `constant = <mask>` in proto.md §6.2's
    /// `ServerFeature` block.
    fn declares(line: &str, constant: &str, mask: u32) -> bool {
        let Some(rest) = line.trim_start().strip_prefix(constant) else {
            return false;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            return false;
        };
        let digits: String = value
            .trim_start()
            .trim_start_matches("0x")
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        u32::from_str_radix(&digits, 16).is_ok_and(|parsed| parsed == mask)
    }

    #[test]
    fn every_known_bit_is_named_after_its_proto_md_constant() {
        let features: Vec<ServerFeature> = NAMES.iter().map(|(feature, _)| *feature).collect();
        assert_eq!(
            ServerFeatureSet::with(&features).as_wire(),
            ServerFeatureSet::from_wire(u32::MAX).as_wire(),
            "every ServerFeature bit this build knows needs a name here"
        );
        let proto = include_str!("../../../docs/spec/proto.md");
        for (feature, name) in NAMES {
            let constant = name.to_ascii_uppercase();
            let mask = *feature as u32;
            assert!(
                proto.lines().any(|line| declares(line, &constant, mask)),
                "{name} is not proto.md's {constant} = {mask:#010x} lower-cased"
            );
        }
        assert_eq!(
            feature_names(ServerFeatureSet::from_wire(u32::MAX)).len(),
            NAMES.len()
        );
    }
}
