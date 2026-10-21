//! Encoding organization commands: the nested tag, then a field-tagged body.

use super::{
    Command, Encoder, LaunchMode, encode_filter, encode_launch, encode_live_target,
    encode_move_item, encode_pair, encode_pin, field, write_document, write_key, write_name,
    write_operation, write_project, write_resource, write_session, write_u8, write_u32, write_u64,
};

/// The nested tag of an organization command, or `None` for another
/// command.
pub(super) const fn org_command_tag(command: &Command) -> Option<u8> {
    use crate::wire::frame::{
        COMMAND_TAG_ARRANGEMENT_GET, COMMAND_TAG_ARRANGEMENT_REPLACE, COMMAND_TAG_GET_ORGANIZATION,
        COMMAND_TAG_INBOX_RESOLVE, COMMAND_TAG_ORGANIZATION_UNDO, COMMAND_TAG_RESOURCE_CREATE,
        COMMAND_TAG_RESOURCE_MOVE, COMMAND_TAG_RESOURCE_MOVE_MANY, COMMAND_TAG_RESOURCE_RELAUNCH,
        COMMAND_TAG_RESOURCE_REMOVE, COMMAND_TAG_RESOURCE_RENAME, COMMAND_TAG_RESOURCE_TERMINATE,
        COMMAND_TAG_SESSION_CREATE, COMMAND_TAG_SESSION_DELETE, COMMAND_TAG_SESSION_RENAME,
        COMMAND_TAG_SESSION_SET_LIFETIME,
    };
    Some(match command {
        Command::GetOrganization { .. } => COMMAND_TAG_GET_ORGANIZATION,
        Command::SessionCreate { .. } => COMMAND_TAG_SESSION_CREATE,
        Command::SessionRename { .. } => COMMAND_TAG_SESSION_RENAME,
        Command::SessionSetLifetime { .. } => COMMAND_TAG_SESSION_SET_LIFETIME,
        Command::SessionDelete { .. } => COMMAND_TAG_SESSION_DELETE,
        Command::ResourceCreate { .. } => COMMAND_TAG_RESOURCE_CREATE,
        Command::ResourceMove { .. } => COMMAND_TAG_RESOURCE_MOVE,
        Command::ResourceMoveMany { .. } => COMMAND_TAG_RESOURCE_MOVE_MANY,
        Command::ResourceRename { .. } => COMMAND_TAG_RESOURCE_RENAME,
        Command::ResourceTerminate { .. } => COMMAND_TAG_RESOURCE_TERMINATE,
        Command::ResourceRemove { .. } => COMMAND_TAG_RESOURCE_REMOVE,
        Command::ResourceRelaunch { .. } => COMMAND_TAG_RESOURCE_RELAUNCH,
        Command::InboxResolve { .. } => COMMAND_TAG_INBOX_RESOLVE,
        Command::OrganizationUndo { .. } => COMMAND_TAG_ORGANIZATION_UNDO,
        Command::ArrangementGet { .. } => COMMAND_TAG_ARRANGEMENT_GET,
        Command::ArrangementReplace { .. } => COMMAND_TAG_ARRANGEMENT_REPLACE,
        Command::DelegateAgent { .. } => crate::wire::frame::COMMAND_TAG_DELEGATE_AGENT,
        Command::RevokeDelegation { .. } => crate::wire::frame::COMMAND_TAG_REVOKE_DELEGATION,
        Command::SessionKill { .. } => crate::wire::frame::COMMAND_TAG_SESSION_KILL,
        Command::SessionSetProject { .. } => crate::wire::frame::COMMAND_TAG_SESSION_SET_PROJECT,
        _ => return None,
    })
}

/// Write an organization command: its nested tag, then its field-tagged
/// body. `false`, writing nothing, for a command that is not an
/// organization command.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per organization command, kept as one flat encode table"
)]
pub(in crate::wire) fn encode_org_command(command: &Command, enc: &mut Encoder<'_>) -> bool {
    let Some(tag) = org_command_tag(command) else {
        return false;
    };
    enc.write_u8(tag);
    match command {
        Command::GetOrganization {
            filter,
            page_size,
            cursor,
        } => {
            enc.write_field_with(field::org::FILTER, |e| encode_filter(*filter, e));
            write_u32(enc, field::org::PAGE_SIZE, *page_size);
            if let Some(cursor) = cursor {
                enc.write_field(field::org::CURSOR, cursor);
            }
        }
        Command::SessionCreate {
            operation,
            name,
            lifetime,
            attrs,
            launch,
        } => {
            write_operation(enc, operation);
            write_name(enc, name);
            write_u8(enc, field::org::LIFETIME, lifetime.to_u8());
            for pair in attrs {
                encode_pair(enc, field::org::ATTR, pair);
            }
            if let Some(launch) = launch {
                enc.write_field_with(field::org::LAUNCH, |e| encode_launch(launch, e));
            }
        }
        Command::SessionRename {
            operation,
            session,
            name,
        } => {
            write_operation(enc, operation);
            write_session(enc, field::org::SESSION, session);
            write_name(enc, name);
        }
        Command::SessionSetProject {
            operation,
            session,
            project,
        } => {
            write_operation(enc, operation);
            write_session(enc, field::org::SESSION, session);
            write_project(enc, project.as_deref());
        }
        Command::SessionSetLifetime {
            operation,
            session,
            lifetime,
        } => {
            write_operation(enc, operation);
            write_session(enc, field::org::SESSION, session);
            write_u8(enc, field::org::LIFETIME, lifetime.to_u8());
        }
        Command::SessionDelete { operation, session }
        | Command::SessionKill { operation, session } => {
            write_operation(enc, operation);
            write_session(enc, field::org::SESSION, session);
        }
        Command::InboxResolve {
            operation,
            session,
            expected,
        } => {
            write_operation(enc, operation);
            write_session(enc, field::org::SESSION, session);
            write_u64(enc, field::org::EXPECTED, expected.get());
        }
        Command::ResourceCreate {
            operation,
            session,
            kind,
            launch,
            name,
            on_exit,
            launch_mode,
        } => {
            write_operation(enc, operation);
            write_session(enc, field::org::SESSION, session);
            if let Some(name) = name {
                write_name(enc, name);
            }
            if let Some(launch) = launch {
                enc.write_field_with(field::org::LAUNCH, |e| encode_launch(launch, e));
            }
            write_u8(enc, field::org::KIND, kind.as_wire());
            write_u8(enc, field::org::ON_EXIT, on_exit.to_u8());
            if *launch_mode != LaunchMode::Immediate {
                write_u8(enc, field::org::LAUNCH_MODE, launch_mode.to_u8());
            }
        }
        Command::ResourceMove {
            operation,
            resource,
            from,
            from_expected,
            to,
            to_expected,
        } => {
            write_operation(enc, operation);
            write_resource(enc, resource);
            write_session(enc, field::org::FROM, from);
            write_u64(enc, field::org::FROM_REVISION, from_expected.get());
            write_session(enc, field::org::TO, to);
            write_u64(enc, field::org::TO_REVISION, to_expected.get());
        }
        Command::ResourceMoveMany {
            operation,
            items,
            to,
            to_expected,
        } => {
            write_operation(enc, operation);
            write_session(enc, field::org::TO, to);
            write_u64(enc, field::org::TO_REVISION, to_expected.get());
            for item in items {
                encode_move_item(item, enc);
            }
        }
        Command::ResourceRename {
            operation,
            resource,
            name,
        } => {
            write_operation(enc, operation);
            write_resource(enc, resource);
            if let Some(name) = name {
                write_name(enc, name);
            }
        }
        Command::ResourceTerminate {
            operation,
            target,
            mode,
        } => {
            write_operation(enc, operation);
            enc.write_field_with(field::org::TARGET, |e| encode_live_target(target, e));
            write_u8(enc, field::org::MODE, mode.to_u8());
        }
        Command::ResourceRemove {
            operation,
            resource,
        } => {
            write_operation(enc, operation);
            write_resource(enc, resource);
        }
        Command::ResourceRelaunch {
            operation,
            resource,
            expected,
        } => {
            write_operation(enc, operation);
            write_resource(enc, resource);
            write_u64(enc, field::org::EXPECTED, expected.get());
        }
        Command::OrganizationUndo {
            operation,
            undo,
            expected,
        } => {
            write_operation(enc, operation);
            write_key(enc, field::org::UNDO, undo.as_bytes());
            for pin in expected {
                encode_pin(pin, enc);
            }
        }
        Command::ArrangementGet { arrangement } => {
            write_key(enc, field::org::ARRANGEMENT, arrangement.as_bytes());
        }
        Command::ArrangementReplace {
            operation,
            arrangement,
            expected,
            document,
        } => {
            write_operation(enc, operation);
            write_u64(enc, field::org::EXPECTED, expected.get());
            write_key(enc, field::org::ARRANGEMENT, arrangement.as_bytes());
            write_document(enc, document);
        }
        Command::DelegateAgent {
            operation,
            session,
            ttl_secs,
        } => {
            write_operation(enc, operation);
            write_session(enc, field::org::SESSION, session);
            write_u32(enc, field::org::TTL_SECS, *ttl_secs);
        }
        Command::RevokeDelegation {
            operation,
            credential_id,
        } => {
            write_operation(enc, operation);
            enc.write_field(field::org::CREDENTIAL, credential_id.as_bytes());
        }
        _ => return false,
    }
    true
}
