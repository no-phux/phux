//! Portable receipt expectations shared by native requests and the retry journal.
use phux_protocol::ids::OperationId;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};
use super::{OrgOutcome, OrganizationCommand};

/// Required evidence for the exact submitted command; no clocks or transport.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum OrgReplyExpectation {
    /// No organization mutation identity is required by this request.
    Read,
    /// A mutation must acknowledge this operation before it can be accepted.
    Mutation(OperationId),
    /// Exact creation must commit this operation and identify its new session.
    SessionCreate(OperationId),
    /// Ensure must identify the session and whether this operation created it.
    SessionEnsure(OperationId),
    /// Resource creation must commit this operation and identify its resource.
    ResourceCreate(OperationId),
    /// Termination retains this operation and its admission/end evidence.
    Terminate(OperationId),
}

impl OrgReplyExpectation {
    /// Capture before handing an owned command to a driver, without cloning it.
    #[must_use]
    pub fn for_command(command: &Command) -> Self {
        match command {
            Command::SessionCreate { operation, .. } => Self::SessionCreate(*operation),
            Command::SessionEnsure { operation, .. } => Self::SessionEnsure(*operation),
            Command::ResourceCreate { operation, .. } => Self::ResourceCreate(*operation),
            Command::ResourceTerminate { operation, .. } => Self::Terminate(*operation),
            _ => command.organization_operation().copied().map_or(Self::Read, Self::Mutation),
        }
    }

    /// Missing required identities/outcomes are uncertainty, never refusal or
    /// permission to create again. A live target is not required: intent may
    /// commit before execution starts, or may deliberately stay dormant.
    #[must_use]
    pub fn classify(self, result: &CommandResult) -> OrgOutcome {
        let operation = match self {
            Self::Read => return OrgOutcome::from_result(result),
            Self::Terminate(operation) => return OrgOutcome::from_terminate_reply(operation, result),
            Self::Mutation(operation) | Self::SessionCreate(operation)
                | Self::SessionEnsure(operation) | Self::ResourceCreate(operation) => operation,
        };
        let problem = match result {
            CommandResult::OkWith(CommandValue::Committed(receipt)) if receipt.operation != operation =>
                Some("the receipt belongs to another operation"),
            CommandResult::OkWith(CommandValue::Committed(receipt))
                if matches!(self, Self::SessionCreate(_) | Self::SessionEnsure(_))
                    && receipt.session.is_none() => Some("the committed receipt omitted its session identity"),
            CommandResult::OkWith(CommandValue::Committed(receipt))
                if matches!(self, Self::SessionEnsure(_)) && receipt.ensured_created.is_none() =>
                    Some("the committed ensure receipt omitted existing/created resolution"),
            CommandResult::OkWith(CommandValue::Committed(receipt))
                if matches!(self, Self::ResourceCreate(_)) && receipt.resource.is_none() =>
                    Some("the committed create receipt omitted its resource identity"),
            CommandResult::OkWith(CommandValue::Committed(_)) => None,
            CommandResult::Accepted { operation: acknowledged } if *acknowledged != operation =>
                Some("the acceptance belongs to another operation"),
            CommandResult::Accepted { .. } | CommandResult::Ok if matches!(self, Self::Mutation(_)) => None,
            CommandResult::Accepted { .. } | CommandResult::Ok | CommandResult::OkWith(_) =>
                Some("the reply did not acknowledge this exact committed operation"),
            _ => None,
        };
        match problem {
            Some(reason) => OrgOutcome::Unknown { reason: reason.to_owned() },
            None => OrgOutcome::from_result(result),
        }
    }
}

#[cfg(test)]
mod tests;
