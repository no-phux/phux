//! Host-serialized ensure behavior in the native scripted peer.
use super::{Inverse, OrgFixture};
use phux_protocol::ids::OperationId;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, Committed, LaunchSpec, Refusal, SessionLifetime,
};

impl OrgFixture {
    pub(super) fn ensure_session(
        &mut self,
        operation: OperationId,
        name: &str,
        lifetime: SessionLifetime,
        launch: Option<&LaunchSpec>,
        attrs: &[(String, String)],
    ) -> Result<(CommandResult, Option<Inverse>), Refusal> {
        if let Some(session) = self.sessions.iter().find(|session| session.name == name) {
            let mut receipt =
                Committed::new(operation, session.revision).with_session(Some(session.key));
            receipt.ensured_created = Some(false);
            return Ok((
                CommandResult::OkWith(CommandValue::Committed(Box::new(receipt))),
                None,
            ));
        }
        let (result, inverse) = self.mutate(&Command::SessionCreate {
            operation,
            name: name.to_owned(),
            lifetime,
            attrs: attrs.to_vec(),
            launch: launch.cloned().map(Box::new),
        })?;
        let CommandResult::OkWith(CommandValue::Committed(mut receipt)) = result else {
            unreachable!("fixture session creation returns a committed receipt");
        };
        receipt.ensured_created = Some(true);
        Ok((
            CommandResult::OkWith(CommandValue::Committed(receipt)),
            inverse,
        ))
    }
}
