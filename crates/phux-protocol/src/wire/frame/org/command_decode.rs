//! Decoding organization commands from their field-tagged bodies.

use super::{Command, DecodeError, Decoder, Fields, TerminateMode, enum_byte, req};

/// Decode the organization command with nested tag `tag` from its
/// field-tagged body, or `None` (reading nothing) for another tag.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per organization command, kept as one flat decode table"
)]
pub(in crate::wire) fn decode_org_command(
    tag: u8,
    dec: &mut Decoder<'_>,
) -> Result<Option<Command>, DecodeError> {
    use crate::wire::frame::{
        COMMAND_TAG_ARRANGEMENT_GET, COMMAND_TAG_ARRANGEMENT_REPLACE, COMMAND_TAG_GET_ORGANIZATION,
        COMMAND_TAG_INBOX_RESOLVE, COMMAND_TAG_ORGANIZATION_UNDO, COMMAND_TAG_RESOURCE_CREATE,
        COMMAND_TAG_RESOURCE_MOVE, COMMAND_TAG_RESOURCE_MOVE_MANY, COMMAND_TAG_RESOURCE_RELAUNCH,
        COMMAND_TAG_RESOURCE_REMOVE, COMMAND_TAG_RESOURCE_RENAME, COMMAND_TAG_RESOURCE_TERMINATE,
        COMMAND_TAG_SESSION_CREATE, COMMAND_TAG_SESSION_DELETE, COMMAND_TAG_SESSION_RENAME,
        COMMAND_TAG_SESSION_SET_LIFETIME,
    };
    let after_reserved = [
        crate::wire::frame::COMMAND_TAG_GET_CREATION,
        crate::wire::frame::COMMAND_TAG_DELEGATE_AGENT,
        crate::wire::frame::COMMAND_TAG_REVOKE_DELEGATION,
        crate::wire::frame::COMMAND_TAG_SESSION_KILL,
        crate::wire::frame::COMMAND_TAG_SESSION_SET_PROJECT,
    ];
    if !(COMMAND_TAG_GET_ORGANIZATION..=COMMAND_TAG_ARRANGEMENT_REPLACE).contains(&tag)
        && !after_reserved.contains(&tag)
    {
        return Ok(None);
    }
    let f = Fields::read_from(dec)?;
    let command = match tag {
        crate::wire::frame::COMMAND_TAG_GET_CREATION => Command::GetCreation {
            operation: f.operation()?,
            session: f.session()?,
        },
        COMMAND_TAG_GET_ORGANIZATION => Command::GetOrganization {
            filter: f.filter.unwrap_or_default(),
            page_size: f.page_size.unwrap_or(0),
            cursor: f.cursor,
        },
        COMMAND_TAG_SESSION_CREATE => Command::SessionCreate {
            operation: f.operation()?,
            lifetime: f.lifetime()?.unwrap_or_default(),
            name: req(f.name)?,
            attrs: f.attrs,
            launch: f.launch,
        },
        COMMAND_TAG_SESSION_RENAME => Command::SessionRename {
            operation: f.operation()?,
            session: f.session()?,
            name: req(f.name)?,
        },
        COMMAND_TAG_SESSION_SET_LIFETIME => Command::SessionSetLifetime {
            operation: f.operation()?,
            session: f.session()?,
            lifetime: req(f.lifetime()?)?,
        },
        COMMAND_TAG_SESSION_DELETE => Command::SessionDelete {
            operation: f.operation()?,
            session: f.session()?,
        },
        COMMAND_TAG_RESOURCE_CREATE => Command::ResourceCreate {
            operation: f.operation()?,
            session: f.session()?,
            on_exit: f.on_exit()?.unwrap_or_default(),
            kind: f.kind.unwrap_or_default(),
            launch_mode: f.launch_mode()?.unwrap_or_default(),
            placement: f.placement,
            launch: f.launch,
            name: f.name,
        },
        COMMAND_TAG_RESOURCE_MOVE => Command::ResourceMove {
            operation: f.operation()?,
            resource: f.resource()?,
            from: req(f.from)?,
            from_expected: req(f.from_revision)?,
            to: req(f.to)?,
            to_expected: req(f.to_revision)?,
        },
        COMMAND_TAG_RESOURCE_MOVE_MANY => Command::ResourceMoveMany {
            operation: f.operation()?,
            to: req(f.to)?,
            to_expected: req(f.to_revision)?,
            items: f.move_items,
        },
        COMMAND_TAG_RESOURCE_RENAME => Command::ResourceRename {
            operation: f.operation()?,
            resource: f.resource()?,
            name: f.name,
        },
        COMMAND_TAG_RESOURCE_TERMINATE => Command::ResourceTerminate {
            operation: f.operation()?,
            target: req(f.target)?,
            mode: enum_byte("TerminateMode", f.mode, TerminateMode::from_u8)?.unwrap_or_default(),
        },
        COMMAND_TAG_RESOURCE_REMOVE => Command::ResourceRemove {
            operation: f.operation()?,
            resource: f.resource()?,
        },
        COMMAND_TAG_RESOURCE_RELAUNCH => Command::ResourceRelaunch {
            operation: f.operation()?,
            resource: f.resource()?,
            expected: f.expected()?,
        },
        COMMAND_TAG_INBOX_RESOLVE => Command::InboxResolve {
            operation: f.operation()?,
            session: f.session()?,
            expected: f.expected()?,
        },
        COMMAND_TAG_ORGANIZATION_UNDO => Command::OrganizationUndo {
            operation: f.operation()?,
            undo: req(f.undo)?,
            expected: f.pins,
        },
        COMMAND_TAG_ARRANGEMENT_GET => Command::ArrangementGet {
            arrangement: req(f.arrangement)?,
        },
        COMMAND_TAG_ARRANGEMENT_REPLACE => Command::ArrangementReplace {
            operation: f.operation()?,
            arrangement: req(f.arrangement)?,
            expected: f.expected()?,
            document: req(f.document)?,
        },
        crate::wire::frame::COMMAND_TAG_DELEGATE_AGENT => Command::DelegateAgent {
            operation: f.operation()?,
            session: f.session()?,
            ttl_secs: f.ttl_secs.unwrap_or(0),
        },
        crate::wire::frame::COMMAND_TAG_REVOKE_DELEGATION => Command::RevokeDelegation {
            operation: f.operation()?,
            credential_id: req(f.credential)?,
        },
        crate::wire::frame::COMMAND_TAG_SESSION_KILL => Command::SessionKill {
            operation: f.operation()?,
            session: f.session()?,
        },
        crate::wire::frame::COMMAND_TAG_SESSION_SET_PROJECT => Command::SessionSetProject {
            operation: f.operation()?,
            session: f.session()?,
            project: f.project,
        },
        _ => return Ok(None),
    };
    Ok(Some(command))
}
