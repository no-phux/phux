//! Level reads of the existing question owners. Only SDK callbacks are actionable.

use phux_agent_session::asked::AskedSource;
use phux_protocol::ids::{AuthorityId, ResourceId, ResourceKey};
use phux_protocol::native_agent::questions::{
    CurrentQuestion, CurrentQuestions, PtyQuestionSource, QuestionContext, QuestionOperationId,
    QuestionReceipt, QuestionRef, QuestionTarget, RespondQuestion,
};
use phux_protocol::native_agent::{
    NativeCommand, NativeCommandResult, NativeDisposition, NativeIntent, NativeQuestion,
    NativeRequest,
};
use phux_protocol::wire::frame::{CommandResult, ErrorCode};

use super::binding::{NativeBinding, fingerprint};
use super::controller::{closed, denied, json, refusal, submit_command};
use crate::runtime::commands::CommandContext;
use crate::state::{ClientId, ServerState};

pub(super) fn handle(ctx: &CommandContext, request: NativeRequest) -> CommandResult {
    match request {
        NativeRequest::CurrentQuestions { target } => ctx.state.with(|s| current(s, &target)),
        NativeRequest::RespondQuestion { answer } => {
            ctx.state.with_mut(|s| respond(s, ctx.client_id, answer))
        }
        NativeRequest::QuestionReceipt {
            question,
            operation_id,
        } => ctx.state.with(|s| receipt(s, &question, operation_id)),
        _ => refusal("not a question request"),
    }
}

fn current(s: &ServerState, target: &QuestionTarget) -> CommandResult {
    let Some(authority) = authority(s) else {
        return refusal("question subject has no durable authority");
    };
    let core = match target {
        QuestionTarget::Handle { resource } => s.idspace.resource_from_wire(resource),
        QuestionTarget::Key {
            authority: expected,
            resource,
        } if *expected == authority => s
            .registry()
            .resource_id(&crate::org_wire::core_resource_key(resource)),
        QuestionTarget::Key { .. } => return wrong_authority(),
    };
    let Some(core) = core else {
        return closed("no such local question subject");
    };
    let Some(record) = s.registry().record(core) else {
        return closed("question subject no longer exists");
    };
    let Some(resource) = crate::org_wire::resource_key(record.key) else {
        return refusal("question subject has no durable identity");
    };
    let (context, questions) = match observations(s, core, authority, resource, record.kind) {
        Ok(current) => current,
        Err(error) => return refusal(error),
    };
    json(&CurrentQuestions {
        schema_version: 1,
        authority,
        resource,
        context,
        questions,
    })
}

fn observations(
    s: &ServerState,
    core: phux_core::ids::ResourceId,
    authority: AuthorityId,
    resource: ResourceKey,
    kind: phux_core::ResourceKind,
) -> Result<(QuestionContext, Vec<CurrentQuestion>), String> {
    let binding = s
        .idspace
        .resource_wire(core)
        .and_then(|wire| s.native_agents().bindings.get(wire));
    let (context, questions) = if let Some(binding) = binding {
        let questions = provider_questions(binding, authority, resource)?;
        (
            QuestionContext::Provider {
                connected: !binding.closed && binding.view.connected,
            },
            questions,
        )
    } else if kind == phux_core::ResourceKind::Terminal && !s.native_owned_terminal(core) {
        (QuestionContext::Pty, pty_questions(s, core))
    } else {
        // The guardian is not a conversation. Never expose the controller's
        // legacy mirrored ask there.
        (QuestionContext::None, Vec::new())
    };
    Ok((context, questions))
}

fn pty_questions(s: &ServerState, core: phux_core::ids::ResourceId) -> Vec<CurrentQuestion> {
    s.asked_observation(core)
        .map(|held| CurrentQuestion::PtyEvidence {
            source: pty_source(held.source),
            id: held.payload.id.clone(),
            question: held.payload.question.clone(),
            suggestions: held.payload.suggestions.clone(),
        })
        .into_iter()
        .collect()
}

fn provider_questions(
    binding: &NativeBinding,
    authority: AuthorityId,
    resource: ResourceKey,
) -> Result<Vec<CurrentQuestion>, String> {
    if binding.closed || !binding.view.connected {
        return Ok(Vec::new());
    }
    binding
        .view
        .questions
        .iter()
        .map(|question| {
            Ok(CurrentQuestion::Provider {
                reference: QuestionRef {
                    authority,
                    conversation: resource,
                    activation: binding.activation.clone(),
                    id: question.id.clone(),
                    revision: fingerprint(question)?,
                },
                question: Box::new(question.clone()),
                response_supported: binding.view.capabilities.questions.is_supported(),
                response_reserved: binding.answered.contains_key(&question.id),
            })
        })
        .collect()
}

const fn pty_source(source: AskedSource) -> PtyQuestionSource {
    match source {
        AskedSource::Scrape => PtyQuestionSource::Screen,
        AskedSource::Sentinel => PtyQuestionSource::Sentinel,
        AskedSource::Hook => PtyQuestionSource::Hook,
        AskedSource::Stream => PtyQuestionSource::Stream,
    }
}

const fn authority(s: &ServerState) -> Option<AuthorityId> {
    AuthorityId::new(*s.org().authority().id.as_bytes())
}

fn wrong_authority() -> CommandResult {
    CommandResult::error(
        ErrorCode::PreconditionFailed,
        "question belongs to another authority",
    )
}

pub(super) fn resolve(
    s: &ServerState,
    question: &QuestionRef,
) -> Result<ResourceId, CommandResult> {
    question.validate().map_err(refusal)?;
    if authority(s) != Some(question.authority) {
        return Err(wrong_authority());
    }
    let Some(wire) = s
        .registry()
        .resource_id(&crate::org_wire::core_resource_key(&question.conversation))
        .and_then(|core| s.idspace.resource_wire(core))
    else {
        return Err(closed("conversation no longer has a controller handle"));
    };
    Ok(wire.clone())
}

pub(super) fn matches_revision(reference: &QuestionRef, question: &NativeQuestion) -> bool {
    fingerprint(question).is_ok_and(|value| value == reference.revision)
}

pub(super) fn respond(
    s: &mut ServerState,
    client: ClientId,
    answer: RespondQuestion,
) -> CommandResult {
    if !s
        .connection_grant(client)
        .is_some_and(|grant| grant.kind() == phux_core::actor::PrincipalKind::Human)
    {
        return denied("only a human principal may respond to a structured question");
    }
    if s.native_agents().sealed {
        return CommandResult::error(
            ErrorCode::Overflow,
            "daemon upgrade cut is sealed; no answer was dispatched",
        );
    }
    let agent = match resolve(s, &answer.question) {
        Ok(agent) => agent,
        Err(error) => return error,
    };
    let command = NativeCommand {
        operation_id: answer.operation_id.get(),
        activation: answer.question.activation.clone(),
        intent: NativeIntent::AnswerExact {
            question: answer.question,
            response: answer.response,
        },
    };
    let result = submit_command(s, client, &agent, command);
    let awaiting_dispatch = result.disposition == NativeDisposition::Queued
        && s.native_agents()
            .bindings
            .get(&agent)
            .is_some_and(|binding| binding.pending.contains(&answer.operation_id.get()));
    json(&QuestionReceipt::new(
        answer.operation_id,
        result,
        Some(awaiting_dispatch),
    ))
}

pub(super) fn receipt(
    s: &ServerState,
    question: &QuestionRef,
    operation_id: QuestionOperationId,
) -> CommandResult {
    let binding = resolve(s, question)
        .ok()
        .and_then(|agent| s.native_agents().bindings.get(&agent));
    let prior = binding
        .and_then(|binding| binding.operations.get(&operation_id.get()))
        .filter(|operation| {
            matches!(&operation.command.intent,
                NativeIntent::AnswerExact { question: original, .. } if original == question)
        });
    let result = prior.map_or_else(
        || NativeCommandResult {
            operation_id: operation_id.get(),
            disposition: NativeDisposition::Unknown,
            detail: Some(
                "no retained receipt for this exact question and operation; do not resend".into(),
            ),
        },
        |prior| prior.result.clone(),
    );
    let awaiting_dispatch = prior
        .and(binding)
        .map(|binding| binding.pending.contains(&operation_id.get()));
    json(&QuestionReceipt::new(
        operation_id,
        result,
        awaiting_dispatch,
    ))
}
