//! Connected-server self-description, not a generic invocation dispatcher.
use crate::state::SharedState;
use phux_protocol::{
    api::{self, PublicOperation},
    native_agent::questions,
    wire::frame::{CommandResult, CommandValue},
};

pub(super) fn describe(state: &SharedState) -> CommandResult {
    let (authority, instance) = state.with(|s| {
        (
            phux_protocol::ids::AuthorityId::new(*s.org().authority().id.as_bytes()),
            api::hex16(s.idspace.instance().as_bytes()),
        )
    });
    let document = serde_json::json!({
        "schema_version": 1,
        "source": "connected_server",
        "authority": authority.map(|key| key.to_string()),
        "instance": instance,
        "authorization": "support is not authorization; every invocation is checked at dispatch",
        "operations": [{
            "name": PublicOperation::TerminalCreate.name(), "supported": true,
            "required_features": ["organization", "create_placement"],
            "required_verbs": ["create", "organize"],
            "scope": "destination session; anchor must belong to the same session",
            "completion": "membership, placement and launch intent committed; execution is an observation, not task completion",
            "input_schema": api::terminal_create_schema(),
            "output_schema": api::terminal_created_schema(),
            "error_schema": api::error_schema(),
            "semantic_validation": api::semantic_validation(),
            "retry": { "identity": "idempotency_key", "scope": "authority organization store",
                "max_age_ms": crate::org_store::RECEIPT_HORIZON_MS, "max_count": crate::org_store::RECEIPT_CAP,
                "after_expiry": "resource.show by saved full durable key; never blindly replay",
                "lost_reply": "outcome_unknown; query terminal.operation on the same authority before considering any identical replay" },
            "retention": "resource retained until explicit removal; failed/completed result survives engine retirement but explicit relaunch replaces it; not timed screen retention"
        }, {
            "name": PublicOperation::TerminalOperation.name(), "supported":true, "required_verbs":["inventory"],
            "input_schema":api::terminal_operation_schema(), "output_schema":api::terminal_created_schema(),
            "error_schema":api::error_schema(),
            "completion":"read-only retained receipt lookup; no effects",
            "retry":{"missing_receipt":"original outcome unknown; resource.show by saved full durable key, never blindly replay"}
        }, {
            "name": PublicOperation::ResourceShow.name(), "supported":true, "required_verbs":["inventory"],
            "required_features":["organization", "durable_resource_inspection"],
            "selector":"full ^<durable-resource-key>",
            "input_schema":api::resource_key_schema(),
            "retry":{"read_only":true},
            "output_schema":api::resource_observed_schema(),
            "completion":"read current resource membership and latest activation without a live engine or creation receipt; removed/out-of-scope records are absent; live-handle selectors keep schema 1"
        }, {
            "name": PublicOperation::ExecutionResult.name(), "supported":true,
            "required_features":["organization", "durable_resource_inspection"],
            "required_verbs":["inventory"],
            "input_schema":api::execution_result_request_schema(),
            "output_schema":api::execution_result_schema(),
            "error_schema":api::error_schema(),
            "completion":"observe the exact activation; timeout, supersession and missing evidence do not establish process exit",
            "retry":{"read_only":true,"scope":"original execution authority"}
        }, {
            "name": PublicOperation::QuestionList.name(), "supported":true,
            "required_features":["current_questions"], "required_verbs":["observe"],
            "input_schema":questions::question_target_schema(),
            "output_schema":questions::current_questions_schema(),
            "completion":"current provider callbacks or advisory PTY evidence; observation is not permission to respond",
            "retry":{"read_only":true}
        }, {
            "name": PublicOperation::QuestionRespond.name(), "supported":true,
            "required_features":["current_questions"], "required_verbs":["observe", "input"],
            "scope":"human principal; exact live provider callback and current input authority",
            "input_schema":questions::respond_question_schema(),
            "output_schema":questions::question_receipt_schema(),
            "completion":"queued is not provider consumption; PTY evidence cannot be answered",
            "retry":{"lost_reply":"read question.receipt on the original authority; do not resend"}
        }, {
            "name": PublicOperation::QuestionReceipt.name(), "supported":true,
            "required_features":["current_questions"], "required_verbs":["observe"],
            "input_schema":questions::question_receipt_request_schema(),
            "output_schema":questions::question_receipt_schema(),
            "completion":"read a retained controller receipt; never submit an answer",
            "retry":{"read_only":true,"missing_receipt":"original outcome unknown; do not resend"}
        }],
        "error_outcomes": api::outcome_schema(),
        "execution_contracts": {
            "terminal.create": "execute argv in a new process; empty argv selects the default shell",
            "run": "submit a POSIX command to an existing shell; input acknowledgement is not execution completion",
            "input": "deliver terminal input; never implies a program or agent accepted it",
            "agent.prompt": "submit an agent turn; acknowledgement and lifecycle completion are separate",
            "resource.wait": "observe execution exit",
            "agent.wait": "observe a lifecycle transition, not proof of task success" },
        "events": "organization events follow durable commit; bounded journal gaps require a level-state reread"
    });
    CommandResult::OkWith(CommandValue::Json(document.to_string()))
}
