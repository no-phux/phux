//! The resource-kind catalog and the closed verb classification
//! ([ADR-0125], `docs/remote.md#who-can-do-what`, `docs/protocol.md#commands-events-resources`).
//!
//! One table serves discovery ([`SERVER_METHODS`], [`SUBSTRATE_METHODS`],
//! [`KINDS`]) and authorization ([`classify_frame`], [`classify_command`]):
//! method entries point at the same [`FRAME_RULES`] / [`COMMAND_RULES`] rows
//! the classifiers return, so the two cannot disagree. Compiled metadata,
//! never wire; reading it grants nothing.
//!
//! Both classifiers match exhaustively, so a new variant does not compile
//! until classified; anything the tables do not name is
//! [`Classification::Deny`].
//!
//! [ADR-0125]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0125-kind-catalog-is-generated-metadata.md

use crate::caps::{ServerFeature, ServerFeatureExt};
use crate::ids::{ResourceId, ResourceKind, SatelliteHost};
use crate::wire::frame::{
    APPROVAL_DECIDE_KEY_PREFIX, APPROVAL_DENY, APPROVAL_KEY_PREFIX, EVENT_TAG_APPROVAL_DECIDED,
    EVENT_TAG_APPROVAL_REQUESTED, TerminalSignal, is_approval_decision,
};
use crate::wire::frame::{
    AttachTarget, COMMAND_TAG_ACQUIRE_INPUT, COMMAND_TAG_APPEND_RESOURCE_OUTPUT,
    COMMAND_TAG_APPLY_INPUT, COMMAND_TAG_ATTACH_RESOURCE, COMMAND_TAG_CHECKOUT,
    COMMAND_TAG_CLOSE_TAB_RESOURCES, COMMAND_TAG_DETACH_CLIENTS, COMMAND_TAG_DETACH_RESOURCE,
    COMMAND_TAG_GET_FILE, COMMAND_TAG_GET_PERF, COMMAND_TAG_GET_SCREEN, COMMAND_TAG_GET_STATE,
    COMMAND_TAG_GET_TERMINAL_STATE, COMMAND_TAG_KILL_RESOURCE, COMMAND_TAG_KILL_RESOURCE_IF,
    COMMAND_TAG_KILL_RESOURCES, COMMAND_TAG_OPEN_LISTENER, COMMAND_TAG_PERF_CONTROL,
    COMMAND_TAG_PUT_FILE, COMMAND_TAG_RELEASE_INPUT, COMMAND_TAG_REPORT_AGENT_STATE,
    COMMAND_TAG_REPORT_ASKED, COMMAND_TAG_ROUTE_INPUT, COMMAND_TAG_SERVICE_HTTP_CLOSE,
    COMMAND_TAG_SERVICE_HTTP_OPEN, COMMAND_TAG_SERVICE_HTTP_READ, COMMAND_TAG_SHUTDOWN,
    COMMAND_TAG_SIGNAL_TERMINAL, COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS, COMMAND_TAG_TRANSCRIBE,
    COMMAND_TAG_UPGRADE, CONFIG_RELOAD_KEY, Command, EVENT_TAG_ASKED, EVENT_TAG_BELL,
    EVENT_TAG_COMMAND_FINISHED, EVENT_TAG_COMMAND_STARTED, EVENT_TAG_CWD_CHANGED, EVENT_TAG_DIRTY,
    EVENT_TAG_IDLE, EVENT_TAG_INPUT_ACTIVITY, EVENT_TAG_JOURNAL_GAP, EVENT_TAG_RESOURCE_CLOSED,
    EVENT_TAG_RESOURCE_SPAWNED, EVENT_TAG_TERMINAL_CONTROL, EVENT_TAG_TITLE_CHANGED, FrameKind,
    RESOURCE_AGENT_KEY, RESOURCE_AGENT_SESSION_KEY, RESOURCE_LINK_KEY, RESOURCE_PANE_OCCUPANT_KEY,
    RESOURCE_TAGS_KEY, SESSION_CREATE_KEY, SESSION_CREATE_RESULT_KEY,
    SESSION_CREATE_RESULT_KEY_PREFIX, SESSION_KEEP_EMPTY_KEY, Scope, SpawnResource, StateScope,
    TYPE_ATTACH, TYPE_COMMAND, TYPE_DELETE_METADATA, TYPE_DETACH, TYPE_FRAME_ACK,
    TYPE_GET_METADATA, TYPE_HELLO, TYPE_HISTORY_REQUEST, TYPE_INPUT_FOCUS, TYPE_INPUT_KEY,
    TYPE_INPUT_MOUSE, TYPE_INPUT_PASTE, TYPE_INPUT_TERMINAL_REPLY, TYPE_LIST_DIRECTORY,
    TYPE_LIST_METADATA, TYPE_MOVE_RESOURCE, TYPE_PATH_QUERY, TYPE_PING, TYPE_RESIZE_TERMINAL,
    TYPE_SET_METADATA, TYPE_SPAWN_RESOURCE, TYPE_SUBSCRIBE_EVENTS, TYPE_SUBSCRIBE_METADATA,
    TYPE_VIEWPORT_RESIZE, WHOAMI_KEY, decode_session_keep_empty,
};
use crate::wire::frame::{
    COMMAND_TAG_ARRANGEMENT_GET, COMMAND_TAG_ARRANGEMENT_REPLACE, COMMAND_TAG_DELEGATE_AGENT,
    COMMAND_TAG_GET_ORGANIZATION, COMMAND_TAG_INBOX_RESOLVE, COMMAND_TAG_ORGANIZATION_UNDO,
    COMMAND_TAG_RESOURCE_CREATE, COMMAND_TAG_RESOURCE_MOVE, COMMAND_TAG_RESOURCE_MOVE_MANY,
    COMMAND_TAG_RESOURCE_RELAUNCH, COMMAND_TAG_RESOURCE_REMOVE, COMMAND_TAG_RESOURCE_RENAME,
    COMMAND_TAG_RESOURCE_TERMINATE, COMMAND_TAG_REVOKE_DELEGATION, COMMAND_TAG_SESSION_CREATE,
    COMMAND_TAG_SESSION_DELETE, COMMAND_TAG_SESSION_KILL, COMMAND_TAG_SESSION_RENAME,
    COMMAND_TAG_SESSION_SET_LIFETIME, COMMAND_TAG_SESSION_SET_PROJECT,
    EVENT_TAG_ACTIVATION_CHANGED, EVENT_TAG_ARRANGEMENT_CHANGED, EVENT_TAG_INTAKE_CHANGED,
    EVENT_TAG_LEASE_CHANGED, EVENT_TAG_MEMBERSHIP_CHANGED, EVENT_TAG_RESOURCE_CREATED,
    EVENT_TAG_RESOURCE_REMOVED, EVENT_TAG_RESOURCE_RENAMED, EVENT_TAG_SESSION_CREATED,
    EVENT_TAG_SESSION_DELETED, EVENT_TAG_SESSION_LIFETIME_CHANGED, EVENT_TAG_SESSION_PROJECT_SET,
    EVENT_TAG_SESSION_RENAMED,
};
use crate::wire::frame::{EVENT_TAG_SOURCE_GAP, SESSION_NAME_KEY};

include!("kinds/classification_types.rs");
include!("kinds/rules.rs");
include!("kinds/classifiers.rs");
include!("kinds/method_catalog.rs");
include!("kinds/resource_catalog.rs");

/// Every catalog method: server-level, then substrate, then each kind's
/// facet in [`KINDS`] order.
pub fn methods() -> impl Iterator<Item = &'static MethodSpec> {
    SERVER_METHODS
        .iter()
        .chain(SUBSTRATE_METHODS)
        .chain(KINDS.iter().flat_map(|kind| kind.methods))
}

/// The catalog method carried by nested command `tag`, or `None` for a tag
/// this build does not allocate (which a decoder refuses and the classifier
/// would deny).
#[must_use]
pub fn command_method(tag: u8) -> Option<&'static MethodSpec> {
    methods().find(|method| method.carrier == Carrier::Command(tag))
}

/// The catalog method carried by client-to-server frame `type_byte`, or
/// `None` for a type that is unallocated, retired, or server-to-client.
#[must_use]
pub fn frame_method(type_byte: u8) -> Option<&'static MethodSpec> {
    methods().find(|method| method.carrier == Carrier::Frame(type_byte))
}

/// The catalog method named `name` (a wire name or a metadata key).
#[must_use]
pub fn method_named(name: &str) -> Option<&'static MethodSpec> {
    methods().find(|method| method.name == name)
}

/// The kind whose facet carries `command`, or `None` for a server or
/// substrate command. Read from the rows authorization uses, so routing and
/// classification cannot disagree.
#[must_use]
pub fn command_kind(command: &Command) -> Option<ResourceKind> {
    let rule = command_rule(command);
    KINDS
        .iter()
        .find(|kind| {
            kind.methods
                .iter()
                .any(|method| method.rules.iter().any(|row| core::ptr::eq(*row, rule)))
        })
        .map(|kind| kind.kind)
}

/// Whether some dangerous method lists `rule` among its rows.
fn rule_is_dangerous(rule: &Rule) -> bool {
    methods()
        .filter(|method| method.dangerous)
        .any(|method| method.rules.iter().any(|row| core::ptr::eq(*row, rule)))
}

/// Whether this instance of a nested command is dangerous (ADR-0128): its
/// method is [`MethodSpec::dangerous`], except `SIGNAL_TERMINAL` with
/// `freeze` or `resume`, the reversible brake.
#[must_use]
pub fn command_is_dangerous(command: &Command) -> bool {
    let reversible = matches!(
        command,
        Command::SignalTerminal {
            signal: TerminalSignal::Freeze | TerminalSignal::Resume,
            ..
        }
    );
    !reversible && rule_is_dangerous(command_rule(command))
}

/// Whether this instance of a client frame is dangerous (ADR-0128): its
/// row belongs to a [`MethodSpec::dangerous`] method, except a decision to
/// `deny`, which releases nothing.
#[must_use]
pub fn frame_is_dangerous(frame: &FrameKind) -> bool {
    if let FrameKind::Command { command, .. } = frame {
        return command_is_dangerous(command);
    }
    let denial = matches!(frame, FrameKind::SetMetadata { value, .. } if value == APPROVAL_DENY);
    !denial && rule_is_dangerous(frame_rule(frame))
}

/// The catalog entry for `kind`, or `None` for a kind this build does not
/// serve.
#[must_use]
pub fn kind_spec(kind: ResourceKind) -> Option<&'static KindSpec> {
    KINDS.iter().find(|spec| spec.kind == kind)
}

#[cfg(test)]
mod samples;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::GroupId;
    use crate::wire::frame::encode_session_keep_empty;

    fn terminal() -> ResourceId {
        ResourceId::local(7)
    }

    fn row(frame: &FrameKind) -> &'static str {
        frame_rule(frame).case
    }

    fn spawn(
        satellite: Option<&str>,
        owner_terminal: Option<ResourceId>,
        resource: Option<SpawnResource>,
    ) -> FrameKind {
        FrameKind::SpawnResource {
            request_id: 1,
            group: GroupId::new(1),
            command: None,
            cwd: None,
            env: None,
            term: None,
            satellite: satellite.map(SatelliteHost::new),
            owner_terminal,
            agent_session: None,
            initial_size: None,
            resource: resource.map(Box::new),
        }
    }

    fn agent(parent: ResourceId) -> SpawnResource {
        SpawnResource::agent_session(parent, "claude")
    }

    #[test]
    fn agent_session_spawn_rows_follow_parent_and_host() {
        assert_eq!(
            row(&spawn(None, None, Some(agent(terminal())))),
            F_SPAWN_AGENT_LOCAL.case
        );
        let remote_parent = ResourceId::satellite("h", 7);
        assert_eq!(
            row(&spawn(Some("h"), None, Some(agent(remote_parent.clone())))),
            F_SPAWN_AGENT_SATELLITE.case
        );
        // A local or different-host parent under a satellite spawn.
        assert_eq!(
            row(&spawn(Some("h"), None, Some(agent(terminal())))),
            F_UNCLASSIFIED.case
        );
        assert_eq!(
            row(&spawn(Some("other"), None, Some(agent(remote_parent)))),
            F_UNCLASSIFIED.case
        );
        // No row covers an agent session that also names an owner.
        assert_eq!(
            row(&spawn(None, Some(terminal()), Some(agent(terminal())))),
            F_UNCLASSIFIED.case
        );
    }

    #[test]
    fn a_kind_that_contradicts_its_binding_is_refused_first() {
        let mut parented_terminal = agent(terminal());
        parented_terminal.kind = ResourceKind::Terminal;
        let mut orphan_agent = agent(terminal());
        orphan_agent.parent = None;
        let mut orphan_unknown = agent(terminal());
        orphan_unknown.kind = ResourceKind::Unknown { tag: 9 };
        orphan_unknown.parent = None;
        for resource in [parented_terminal, orphan_agent, orphan_unknown] {
            assert_eq!(
                row(&spawn(None, None, Some(resource))),
                F_SPAWN_KIND_MISMATCH.case
            );
        }
    }

    fn set(scope: Scope, key: &str, value: &[u8]) -> FrameKind {
        FrameKind::SetMetadata {
            request_id: 1,
            scope,
            key: key.to_owned(),
            value: value.to_vec(),
        }
    }

    fn delete(key: &str) -> FrameKind {
        FrameKind::DeleteMetadata {
            request_id: 1,
            scope: Scope::Global,
            key: key.to_owned(),
        }
    }

    fn subscribe(key: &str) -> FrameKind {
        FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: key.to_owned(),
        }
    }

    #[test]
    fn server_interpreted_global_writes_have_their_own_rows() {
        assert_eq!(
            row(&set(Scope::Global, SESSION_CREATE_KEY, b"{}")),
            F_SESSION_CREATE.case
        );
        assert_eq!(
            row(&set(Scope::Global, CONFIG_RELOAD_KEY, b"1")),
            F_CONFIG_RELOAD.case
        );
        let mark = encode_session_keep_empty("work", true);
        let unmark = encode_session_keep_empty("work", false);
        assert_eq!(
            row(&set(Scope::Global, SESSION_KEEP_EMPTY_KEY, &mark)),
            F_KEEP_EMPTY_MARK.case
        );
        assert_eq!(
            row(&set(Scope::Global, SESSION_KEEP_EMPTY_KEY, &unmark)),
            F_KEEP_EMPTY_CLEAR.case
        );
        for other in [b"work".as_slice(), b"work\0yes".as_slice()] {
            assert_eq!(
                row(&set(Scope::Global, SESSION_KEEP_EMPTY_KEY, other)),
                F_KEEP_EMPTY_OTHER.case
            );
        }
        // The special rows are Global-scoped; elsewhere the key is ordinary.
        assert_eq!(
            row(&set(Scope::Resource(terminal()), SESSION_CREATE_KEY, b"{}")),
            F_METADATA_WRITE.case
        );
    }

    #[test]
    fn server_owned_keys_are_not_writable() {
        let result = format!("{SESSION_CREATE_RESULT_KEY_PREFIX}token");
        for key in [SESSION_CREATE_RESULT_KEY, result.as_str()] {
            assert_eq!(
                row(&set(Scope::Global, key, b"x")),
                F_RESULT_NAMESPACE_WRITE.case
            );
            assert_eq!(row(&delete(key)), F_RESULT_NAMESPACE_WRITE.case);
            assert_eq!(row(&subscribe(key)), F_RESULT_NAMESPACE_SUBSCRIBE.case);
        }
        for key in [
            RESOURCE_PANE_OCCUPANT_KEY,
            WHOAMI_KEY,
            crate::wire::frame::RESOURCE_PROGRAM_STATUS_KEY,
            crate::wire::frame::RESOURCE_PROGRAM_STATUS_RECORD_PREFIX,
            "phux.program-status.record/v1/build/test",
        ] {
            assert_eq!(
                row(&set(Scope::Resource(terminal()), key, b"x")),
                F_SERVER_OWNED_WRITE.case
            );
            assert_eq!(row(&delete(key)), F_SERVER_OWNED_WRITE.case);
        }
        for key in [CONFIG_RELOAD_KEY, SESSION_KEEP_EMPTY_KEY] {
            assert_eq!(row(&delete(key)), F_SERVER_OWNED_WRITE.case);
        }
        assert_eq!(row(&delete(SESSION_CREATE_KEY)), F_METADATA_WRITE.case);
        assert_eq!(
            row(&set(Scope::Resource(terminal()), RESOURCE_TAGS_KEY, b"[]")),
            F_METADATA_WRITE.case
        );
        assert_eq!(
            row(&subscribe(RESOURCE_AGENT_KEY)),
            F_SUBSCRIBE_METADATA.case
        );
    }

    #[test]
    fn verb_set_helpers() {
        let all = Verbs::of(&Verb::ALL);
        assert_eq!(all.bits(), Verbs::KNOWN_BITS);
        assert!(!Verbs::of(&[Verb::Inventory, Verb::Observe]).mutates());
        assert!(Verbs::of(&[Verb::Observe, Verb::Bind]).mutates());
        assert!(Verbs::EMPTY.is_empty());
        assert_eq!(
            C_GET_PERF_RESET.requirement_label(),
            "OBSERVE+BIND",
            "labels list verbs in bit order"
        );
        assert_eq!(
            C_OPEN_LISTENER.requirement_label(),
            "SIGNAL + owner-UDS transport"
        );
    }
}
