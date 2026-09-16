//! The process's declared attach intent (ADR-0127): `phux attach --viewer`
//! or `--take`, set once by the CLI before it dials and read by every
//! `ATTACH` this process sends, so a reconnect or an in-TUI session switch
//! keeps the role the operator asked for.

use std::sync::atomic::{AtomicU8, Ordering};

use phux_protocol::caps::ServerFeature;
use phux_protocol::wire::frame::RolePolicy;

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// The declared role as its wire byte. `0` is `{ PRIMARY, NEVER }`, the
/// default, which sends no field at all.
static ATTACH_ROLE: AtomicU8 = AtomicU8::new(0);

/// Declare the role every later `ATTACH` from this process carries.
pub fn set_attach_role(policy: RolePolicy) {
    ATTACH_ROLE.store(policy.to_u8(), Ordering::Relaxed);
}

/// The `role_policy` field for an `ATTACH` on `conn`.
///
/// `None` for the default, so the frame is byte-identical to one from before
/// roles. A declared role needs a server that honors it: an older one would
/// ignore the field and grant an ordinary, input-capable attach, so the
/// attach is refused here instead of silently widening.
pub(super) fn attach_role_for(conn: &Connection) -> Result<Option<RolePolicy>, AttachError> {
    let policy = RolePolicy::from_u8(ATTACH_ROLE.load(Ordering::Relaxed));
    let field = role_field(policy, advertises_roles(conn))?;
    // A takeover is one deliberate act: a reconnect or a session switch later
    // must not seize again (L1 §8.1), so it is consumed by this attach.
    if policy.takes_over() {
        set_attach_role(RolePolicy::PRIMARY);
    }
    Ok(field)
}

/// The role a Terminal this process attaches on its own, after the session
/// attach, declares: a viewer stays a viewer on every pane it opens, and
/// nothing else rides a per-pane attach. The session attach already proved
/// the server honors roles, or it would have been refused.
pub(super) fn pane_attach_role() -> Option<RolePolicy> {
    let policy = RolePolicy::from_u8(ATTACH_ROLE.load(Ordering::Relaxed));
    policy.is_viewer().then_some(policy)
}

fn advertises_roles(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features
            .contains(ServerFeature::AttachRoles)
    })
}

fn role_field(policy: RolePolicy, honored: bool) -> Result<Option<RolePolicy>, AttachError> {
    if policy == RolePolicy::PRIMARY {
        return Ok(None);
    }
    if honored {
        return Ok(Some(policy));
    }
    Err(AttachError::Refused(
        "this server predates attach roles (ATTACH_ROLES): --viewer and --take need a newer \
         server; attach without them, or upgrade the server"
            .to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_sends_no_field_and_a_declared_role_needs_the_bit() {
        assert!(matches!(role_field(RolePolicy::PRIMARY, false), Ok(None)));
        assert!(matches!(
            role_field(RolePolicy::VIEWER, true),
            Ok(Some(RolePolicy::VIEWER))
        ));
        assert!(matches!(
            role_field(RolePolicy::TAKEOVER, false),
            Err(AttachError::Refused(_))
        ));
    }
}
