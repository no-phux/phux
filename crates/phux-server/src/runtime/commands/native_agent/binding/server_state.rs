//! The server state's native agent slot: lookup, host identity, upgrade
//! recovery, archived guardian relaunch, and provider recipe persistence.

use std::sync::Arc;

use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::{NATIVE_HOST_SPEC_ENV, NativeView};
use tokio::sync::Notify;

use super::validate::{reconcile_publication, restore_operations};
use super::{NO_AGENTS, NativeAgents};
use crate::state::{ClientId, ServerState};

impl ServerState {
    pub(crate) fn native_agents(&self) -> &NativeAgents {
        self.app_slot::<NativeAgents>().unwrap_or(&NO_AGENTS)
    }
    pub(crate) fn native_agents_mut(&mut self) -> &mut NativeAgents {
        self.app_slot_mut::<NativeAgents>()
    }
    pub(crate) fn native_owned_terminal(&self, parent: phux_core::ids::ResourceId) -> bool {
        self.registry().record(parent).is_some_and(|record| matches!(&record.launch,
            Some(phux_core::LaunchSpec::Terminal(launch)) if launch.env.iter().any(|(key, _)| key == NATIVE_HOST_SPEC_ENV)))
            || self.idspace.resource_wire(parent).cloned().is_some_and(|wire| self.native_agents().bindings.values().any(|binding| binding.parent == wire))
    }
    pub(crate) fn native_host_identity(&self, client: ClientId) -> Option<(String, u32)> {
        let peer = self.clients().peer_identity(client)?;
        if peer.transport != phux_protocol::policy::TransportType::UnixSocket
            || peer.uid != rustix::process::geteuid().as_raw()
        {
            return None;
        }
        let credential = self.clients().authenticated_credential(client)?;
        crate::runtime::delegation::delegated_credential(peer, Some(credential))?;
        Some((credential.id.clone(), peer.pid?))
    }
    pub(crate) fn restore_native_agents(&mut self, blob: &crate::upgrade::blob::StateBlob) {
        *self.native_agents_mut() = NativeAgents::default();
        for carried in &blob.native_agents {
            if self.idspace.resource_from_wire(&carried.agent).is_none()
                || self.idspace.resource_from_wire(&carried.parent).is_none()
            {
                continue;
            }
            let mut binding = carried.clone();
            binding.generation = binding.generation.saturating_add(1);
            // Connection grants do not cross a re-exec. Undispatched
            // reservations are refused, not silently executed as a new client.
            // Dispatched operations remain ambiguous until the durable host
            // supplies its operation-ledger receipt.
            restore_operations(&mut binding);
            reconcile_publication(&mut binding, blob);
            if let Ok(bytes) = serde_json::to_vec(&binding.launch) {
                self.metadata_set(
                    &phux_protocol::wire::frame::Scope::Resource(binding.parent.clone()),
                    phux_protocol::wire::frame::RESOURCE_NATIVE_LAUNCH_KEY,
                    bytes,
                );
            }
            let agents = self.native_agents_mut();
            agents
                .wakes
                .insert(binding.agent.clone(), Arc::new(Notify::new()));
            agents.bindings.insert(binding.agent.clone(), binding);
        }
    }
    /// Reserve an activation on an imported dormant terminal without changing
    /// its organization key, membership, name, or arrangement position.
    pub(crate) fn relaunch_native_guardian(
        &mut self,
        guardian: phux_core::ids::ResourceId,
        launch: &phux_core::TerminalLaunch,
        client: ClientId,
        operation: Option<phux_core::OperationId>,
        expected: Option<phux_core::Revision>,
    ) -> Result<(), phux_core::Refusal> {
        let record = self
            .registry()
            .record(guardian)
            .ok_or(phux_core::Refusal::NotFound)?
            .clone();
        if record.kind != phux_core::ResourceKind::Terminal
            || self.resources().handle(guardian).is_some()
            || record.activation.as_ref().is_some_and(|activation| {
                activation.state.is_current() || activation.state == phux_core::ExecState::Lost
            })
        {
            return Err(phux_core::Refusal::InvalidArgument { field: "guardian", reason: "native insertion requires a dormant terminal without a current, lost, or retained engine".into() });
        }
        let command = phux_core::Command {
            operation: operation.unwrap_or_else(|| self.mint_operation()),
            op: phux_core::Op::ResourceRelaunch {
                resource: record.key,
                expected: expected.unwrap_or(record.revision),
                launch: Some(phux_core::LaunchSpec::Terminal(launch.clone())),
            },
        };
        let actor = self.actor_for(client);
        self.commit_command_from(&command, &actor, Some(client))?;
        Ok(())
    }
    /// Persist only a factual native provider identity, after its stream
    /// publication landed. No producer can change lifecycle or arrangement.
    pub(crate) fn persist_native_recipe(
        &mut self,
        agent: &ResourceId,
        view: &NativeView,
    ) -> Result<(), phux_core::Refusal> {
        let binding = self
            .native_agents()
            .bindings
            .get(agent)
            .ok_or(phux_core::Refusal::NotFound)?;
        let Some(native_id) = &view.provider_session_id else {
            return Ok(());
        };
        if binding.launch.native_id.as_ref() == Some(native_id) {
            return Ok(());
        }
        let mut recipe = binding.launch.clone();
        recipe.native_id = Some(native_id.clone());
        let parent = self
            .idspace
            .resource_from_wire(&binding.parent)
            .ok_or(phux_core::Refusal::NotFound)?;
        let record = self
            .registry()
            .record(parent)
            .ok_or(phux_core::Refusal::NotFound)?
            .clone();
        let activation = record
            .activation
            .as_ref()
            .ok_or(phux_core::Refusal::NotFound)?
            .id;
        let Some(phux_core::LaunchSpec::Terminal(mut launch)) = record.launch else {
            return Err(phux_core::Refusal::InvalidArgument {
                field: "launch",
                reason: "native guardian is not a terminal launch".into(),
            });
        };
        let encoded = serde_json::to_string(&recipe).map_err(|error| {
            phux_core::Refusal::InvalidArgument {
                field: "native_recipe",
                reason: error.to_string(),
            }
        })?;
        let Some((_, value)) = launch
            .env
            .iter_mut()
            .find(|(key, _)| key == NATIVE_HOST_SPEC_ENV)
        else {
            return Err(phux_core::Refusal::InvalidArgument {
                field: "native_recipe",
                reason: "native guardian has no explicit recipe marker".into(),
            });
        };
        *value = encoded;
        self.commit_system(phux_core::Op::ResourceLaunchReported {
            resource: record.key,
            activation,
            launch: phux_core::LaunchSpec::Terminal(launch),
        })?;
        if let Some(binding) = self.native_agents_mut().bindings.get_mut(agent) {
            binding.launch = recipe;
        }
        Ok(())
    }
}
