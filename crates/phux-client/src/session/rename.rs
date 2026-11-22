use phux_protocol::ids::SessionId;
use phux_protocol::wire::frame::{OrgFilter, Refusal};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::rename::{NamedSession, RenamePlan, RenameRefusal};

/// Why [`rename_checked`] refused to send the write.
#[derive(Debug, thiserror::Error)]
pub enum RenameError {
    /// `session` is not in the snapshot. Session names are hub-local.
    #[error("no such session")]
    NoSuchSession,
    /// `new_name` is already held by another session.
    #[error("{new_name:?} already exists")]
    AlreadyExists {
        /// The name that collided.
        new_name: String,
    },
    /// `new_name` could not be typed back as a session selector.
    #[error(transparent)]
    InvalidName(#[from] crate::rename::SessionNameError),
    /// The session was deleted before the rename reached it.
    #[error("the session no longer exists")]
    SessionGone,
    /// The server answered the rename with neither a commit nor a refusal.
    #[error("the server did not rename the session (the name may have been taken meanwhile)")]
    NotApplied,
    /// The server refused the `SESSION_RENAME`.
    #[error("{}", crate::org::refusal_sentence(.0))]
    Refused(Refusal),
    /// An organization request failed outright.
    #[error(transparent)]
    Org(crate::org::OrgError),
    /// Transport or decode failure.
    #[error(transparent)]
    Attach(#[from] AttachError),
}

impl From<RenameRefusal> for RenameError {
    fn from(refusal: RenameRefusal) -> Self {
        match refusal {
            RenameRefusal::NoSuchSession => Self::NoSuchSession,
            RenameRefusal::AlreadyExists { new_name } => Self::AlreadyExists { new_name },
            RenameRefusal::InvalidName(invalid) => Self::InvalidName(invalid),
        }
    }
}

impl RenameError {
    /// The refusal reason both surfaces interpolate after `rename refused
    /// for session …:`.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::NoSuchSession => "no such session".to_owned(),
            Self::AlreadyExists { new_name } => format!("{new_name:?} already exists"),
            Self::InvalidName(_)
            | Self::SessionGone
            | Self::NotApplied
            | Self::Refused(_)
            | Self::Org(_)
            | Self::Attach(_) => self.to_string(),
        }
    }
}

/// The checked rename: the shared [`crate::rename`] policy on this connection.
///
/// Refuses against fresh organization records and skips a no-op, then sends
/// one `SESSION_RENAME`, last writer wins; the reply is the outcome.
/// Inventory notices are appended to `notices`.
///
/// # Errors
///
/// [`RenameError`] — see its variants.
pub async fn rename_checked(
    conn: &mut Connection,
    session: &str,
    new_name: &str,
    notices: &mut Vec<String>,
) -> Result<(), RenameError> {
    let inventory = crate::org::fetch(conn, OrgFilter::All, 1)
        .await
        .map_err(RenameError::Org)?;
    notices.extend(inventory.degradation.notices().iter().cloned());
    let roster: Vec<NamedSession<'_>> = inventory
        .sessions
        .iter()
        .enumerate()
        .map(|(index, record)| NamedSession {
            id: record
                .live
                .unwrap_or_else(|| SessionId::new(u32::MAX - u32::try_from(index).unwrap_or(0))),
            name: record.name.as_str(),
        })
        .collect();
    let target = match crate::rename::plan_rename(&roster, session, new_name) {
        RenamePlan::Unchanged { .. } => return Ok(()),
        RenamePlan::Refused(refusal) => return Err(refusal.into()),
        RenamePlan::Send { .. } => inventory
            .session_named(session)
            .ok_or(RenameError::NoSuchSession)?,
    };
    let command =
        crate::rename::org_rename_command(crate::org::mint_operation(), target.key, new_name);
    let (outcome, degradation) = crate::org::request(conn, 2, command)
        .await
        .map_err(RenameError::Org)?;
    notices.extend(degradation.notices().iter().cloned());
    match outcome {
        crate::org::OrgOutcome::Committed(_) | crate::org::OrgOutcome::Accepted => Ok(()),
        crate::org::OrgOutcome::Refused(Refusal::NotFound) => Err(RenameError::SessionGone),
        crate::org::OrgOutcome::Refused(Refusal::NameTaken) => Err(RenameError::AlreadyExists {
            new_name: new_name.to_owned(),
        }),
        crate::org::OrgOutcome::Refused(refusal) => Err(RenameError::Refused(refusal)),
        _ => Err(RenameError::NotApplied),
    }
}
