use super::closure::CLOSURE_PUBLICATION_ID;
use super::controller::{authenticated_host_binding, closed, json, refusal};
use crate::runtime::commands::CommandContext;
use crate::state::SharedState;
use crate::state::native_agent::NativeBinding;
use crate::state::native_agent::NativePublication;
use crate::state::native_agent::fingerprint;
use bytes::Bytes;
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::MAX_NATIVE_OPERATIONS;
use phux_protocol::native_agent::NATIVE_SCHEMA;
use phux_protocol::native_agent::NativePublicationReceipt;
use phux_protocol::native_agent::NativeUpdate;
use phux_protocol::native_agent::NativeView;
use phux_protocol::wire::frame::CommandResult;
use phux_protocol::wire::frame::ErrorCode;

pub(super) async fn publish(
    ctx: &CommandContext,
    agent: ResourceId,
    activation: String,
    proof: String,
    generation: u64,
    id: u64,
    update: NativeUpdate,
) -> CommandResult {
    if id == 0 || id == CLOSURE_PUBLICATION_ID {
        return refusal(
            "publication identity must be nonzero and below the reserved closure identity",
        );
    }
    let fingerprint = match fingerprint(&update) {
        Ok(fingerprint) => fingerprint,
        Err(error) => return refusal(error),
    };
    let projected = ctx.state.with_mut(|s| {
        let binding = authenticated_host_binding(
            s,
            ctx.client_id,
            &agent,
            &activation,
            &proof,
            Some(generation),
        )?;
        reserve_publication(binding, id, &fingerprint, &update)
    });
    let view = match projected {
        Ok(view) => view,
        Err(result) => return result,
    };
    let bytes = match encode_publication(&update, &view) {
        Ok(bytes) => bytes,
        Err(error) => return error,
    };
    let accepted = crate::runtime::apps::agent_session::append::append_native_publication(
        &ctx.state,
        &agent,
        bytes,
        crate::resource::agent_session::NativeAppendIdentity { id, fingerprint },
    )
    .await;
    let accepted = match accepted {
        Ok(accepted) => accepted,
        Err(error) => {
            rollback_publication(&ctx.state, &agent, id);
            return error;
        }
    };
    let receipt = NativePublicationReceipt {
        publication_id: id,
        first_seq: accepted.first_seq,
        last_seq: accepted.last_seq,
    };
    acknowledge_publication(&ctx.state, &agent, view, generation, &receipt, &update)
}

pub(super) fn acknowledge_publication(
    state: &SharedState,
    agent: &ResourceId,
    view: NativeView,
    generation: u64,
    receipt: &NativePublicationReceipt,
    update: &NativeUpdate,
) -> CommandResult {
    let changed = match persist_publication(
        state,
        agent,
        receipt.publication_id,
        view,
        generation,
        receipt,
    ) {
        Ok(changed) => changed,
        Err(error) => {
            return CommandResult::Error {
                code: ErrorCode::Overflow,
                message: format!("native publication accepted but durable recipe pending: {error}"),
            };
        }
    };
    // A question closing while the turn remains blocked must still
    // update the ask inventory, rather than leaving a stale question.
    if changed
        && matches!(
            update,
            NativeUpdate::QuestionOpened { .. }
                | NativeUpdate::QuestionClosed { .. }
                | NativeUpdate::View { .. }
        )
    {
        sync_ask_inventory(state, agent);
    }
    json(receipt)
}

pub(super) fn reserve_publication(
    binding: &mut NativeBinding,
    id: u64,
    fingerprint: &str,
    update: &NativeUpdate,
) -> Result<NativeView, CommandResult> {
    if let Some(prior) = binding
        .publications
        .iter()
        .find(|publication| publication.id == id)
    {
        if prior.fingerprint != fingerprint {
            return Err(refusal("publication identity conflicts with its payload"));
        }
        if let Some(receipt) = &prior.receipt {
            return Err(json(receipt));
        }
        // The actor, not the requesting connection, owns append dedupe.
        // A cancelled/lost acknowledgment can retry exactly this identity.
        return prior
            .view
            .clone()
            .ok_or_else(|| refusal("pending publication has no saved projection"));
    }
    if binding.closed {
        return Err(closed("native agent session is closed"));
    }
    if id <= binding.publication_high_water {
        return Err(refusal(
            "publication receipt expired; old identity cannot be replayed",
        ));
    }
    if binding
        .publications
        .back()
        .is_some_and(|publication| publication.receipt.is_none())
    {
        return Err(refusal("prior publication still pending"));
    }
    let view = binding.project(update).map_err(refusal)?;
    binding.publications.push_back(NativePublication {
        id,
        fingerprint: fingerprint.into(),
        receipt: None,
        view: Some(view.clone()),
    });
    binding.publication_high_water = id;
    while binding.publications.len() > MAX_NATIVE_OPERATIONS {
        binding.publications.pop_front();
    }
    Ok(view)
}

pub(super) fn encode_publication(
    update: &NativeUpdate,
    view: &NativeView,
) -> Result<Bytes, CommandResult> {
    let mut records = Vec::new();
    records.push(
        serde_json::json!({"type":"provider_raw","data":{"schema":NATIVE_SCHEMA,"update":update}}),
    );
    match update {
        NativeUpdate::State { state, .. } => {
            records.push(serde_json::json!({"type":"state","data":{"state":state}}));
        }
        NativeUpdate::QuestionOpened { question } => {
            let text = if question.message.is_empty() {
                &question.title
            } else {
                &question.message
            };
            records.push(serde_json::json!({"type":"ask","data":{"id":question.id,"question":text,"suggestions":question.options.iter().map(|option| &option.label).collect::<Vec<_>>()}}));
        }
        NativeUpdate::Transcript { entry } => {
            records.push(serde_json::json!({"type":"provider_raw","data":entry}));
        }
        NativeUpdate::Notice { level, text } => {
            records.push(
                serde_json::json!({"type":"notification","data":{"level":level,"message":text}}),
            );
        }
        _ => {}
    }
    if !matches!(update, NativeUpdate::View { .. }) {
        records.push(serde_json::json!({"type":"provider_raw","data":{"schema":NATIVE_SCHEMA,"update":{"type":"view","view":view}}}));
    }
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, &record).map_err(|error| refusal(error.to_string()))?;
        bytes.push(b'\n');
    }
    Ok(Bytes::from(bytes))
}

fn persist_publication(
    state: &SharedState,
    agent: &ResourceId,
    id: u64,
    view: NativeView,
    generation: u64,
    receipt: &NativePublicationReceipt,
) -> Result<bool, phux_core::Refusal> {
    state.with_mut(|s| {
        let Some(binding) = s.native_agents().bindings.get(agent) else {
            return Ok(false);
        };
        let Some(publication) = binding
            .publications
            .iter()
            .find(|publication| publication.id == id)
        else {
            return Ok(false);
        };
        if publication.receipt.is_some() || id <= binding.publication_committed {
            // Duplicate acknowledgments are receipts, never projection writes.
            return Ok(false);
        }
        if binding.activation != view.activation {
            return Ok(false);
        }
        let applies = !binding.closed && binding.generation == generation;
        // An actor-accepted stable provider identity is durable launch intent,
        // even if source closure won the projection race. Never reopen controls.
        s.persist_native_recipe(agent, &view)?;
        let Some(binding) = s.native_agents_mut().bindings.get_mut(agent) else {
            return Ok(false);
        };
        if let Some(publication) = binding
            .publications
            .iter_mut()
            .find(|publication| publication.id == id)
        {
            publication.receipt = Some(receipt.clone());
            publication.view = None;
        }
        binding.publication_committed = id;
        if applies {
            binding.view = view;
        }
        update_native_inventory(s, agent);
        Ok(applies)
    })
}

fn update_native_inventory(s: &mut crate::state::ServerState, agent: &ResourceId) {
    let Some(binding) = s.native_agents().bindings.get(agent) else {
        return;
    };
    let native_id = binding.launch.native_id.clone();
    let parent = binding.parent.clone();
    let recipe = serde_json::to_vec(&binding.launch).ok();
    if let Some(core) = s.resource_from_wire(agent)
        && let Some(facet) = s
            .registry_mut()
            .resource_mut(core)
            .and_then(|resource| resource.agent.as_mut())
    {
        facet.native_id = native_id;
    }
    if let Some(bytes) = recipe {
        s.metadata_set(
            &phux_protocol::wire::frame::Scope::Resource(parent),
            "phux.native-launch/v1",
            bytes,
        );
    }
    if let Some(wake) = s.native_agents().wakes.get(agent) {
        wake.notify_waiters();
    }
}

fn rollback_publication(state: &SharedState, agent: &ResourceId, id: u64) {
    state.with_mut(|s| {
        if let Some(binding) = s.native_agents_mut().bindings.get_mut(agent)
            && binding
                .publications
                .back()
                .is_some_and(|publication| publication.id == id && publication.receipt.is_none())
        {
            binding.publications.pop_back();
            binding.publication_high_water = binding
                .publications
                .back()
                .map_or(0, |publication| publication.id);
        }
        if let Some(wake) = s.native_agents().wakes.get(agent) {
            wake.notify_waiters();
        }
    });
}
pub(super) fn sync_ask_inventory(state: &SharedState, agent: &ResourceId) {
    let published = state.with_mut(|s| {
        let binding = s.native_agents().bindings.get(agent)?;
        let parent = binding.parent.clone();
        let question = binding.view.questions.last().cloned();
        let core = s.resource_from_wire(&parent)?;
        let payload = if let Some(question) = question {
            let text = if question.message.is_empty() {
                question.title
            } else {
                question.message
            };
            s.report_stream_ask(
                core,
                phux_resource::agent_asked::AskedPayload {
                    id: question.id,
                    question: text,
                    suggestions: question
                        .options
                        .into_iter()
                        .map(|option| option.label)
                        .collect(),
                    elapsed_seconds: None,
                },
            )
            .emit_payload()
        } else {
            s.retract_agent_asked(core, phux_resource::agent_asked::AskedSource::Stream);
            None
        };
        crate::hub::metadata_mirror::publish_asked_flag(s, &parent, s.agent_is_asked(core));
        payload.map(|payload| (parent, payload))
    });
    if let Some((parent, payload)) = published {
        crate::runtime::client::broadcast_event(state, Some(&parent), &payload.into_event());
    }
}
