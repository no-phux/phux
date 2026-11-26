//! Link-owned resource kind, control class, and bootstrap-profile lookups.

use phux_protocol::caps::BootstrapProfile;

use super::{Command, CommandResult, ErrorCode, RelayHandle};

impl RelayHandle {
    pub(crate) fn known_resource_kind(
        &self,
        terminal: u32,
    ) -> Option<phux_protocol::ids::ResourceKind> {
        let catalog = self
            .resource_kinds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.epoch.is_some_and(|epoch| epoch != catalog.epoch) {
            return None;
        }
        catalog.kinds.get(&terminal).copied()
    }

    pub(crate) fn needs_kind_for_profile(&self, profile: BootstrapProfile) -> bool {
        let catalog = self
            .resource_kinds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.epoch.is_none_or(|epoch| epoch == catalog.epoch)
            && catalog
                .terminal_profile
                .is_some_and(|selected| selected != profile)
    }

    /// Read the connection's known kind, falling back to the existing bounded
    /// inventory request for a direct target never previously enumerated.
    pub(crate) async fn resource_kind(
        &self,
        terminal: u32,
    ) -> Result<phux_protocol::ids::ResourceKind, CommandResult> {
        if !self.link_is_current() {
            return Err(self.expired_link());
        }
        if let Some(kind) = self.known_resource_kind(terminal) {
            return Ok(kind);
        }
        match self
            .command(Command::GetState {
                scope: phux_protocol::wire::frame::StateScope::Server,
            })
            .await
        {
            CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::State(_)) => {
                if !self.link_is_current() {
                    return Err(self.expired_link());
                }
                self.known_resource_kind(terminal).ok_or_else(|| {
                    CommandResult::error(
                        ErrorCode::TerminalNotFound,
                        format!("unknown satellite resource: {terminal}"),
                    )
                })
            }
            refusal => Err(refusal),
        }
    }

    /// A satellite resource's control class, as the satellite's inventory
    /// reports it, fetching that inventory once when this link has not
    /// enumerated it yet. `Ok(None)` for a resource with no class, or one
    /// the inventory does not hold (the relay itself then refuses it).
    pub(crate) async fn resource_control(
        &self,
        terminal: u32,
    ) -> Result<Option<phux_protocol::wire::frame::ControlClass>, CommandResult> {
        if !self.link_is_current() {
            return Err(self.expired_link());
        }
        let known = |handle: &Self| {
            handle
                .resource_kinds
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .controls
                .get(&terminal)
                .copied()
        };
        if let Some(class) = known(self) {
            return Ok(class);
        }
        match self
            .command(Command::GetState {
                scope: phux_protocol::wire::frame::StateScope::Server,
            })
            .await
        {
            CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::State(_)) => {
                if !self.link_is_current() {
                    return Err(self.expired_link());
                }
                Ok(known(self).flatten())
            }
            refusal => Err(refusal),
        }
    }
}
