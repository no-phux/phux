use super::*;
use phux_client::attach::AttachError;

#[test]
fn data_defaults_to_an_empty_object_and_refuses_non_json() {
    assert_eq!(read_data(None).unwrap(), serde_json::json!({}));
    assert_eq!(
        read_data(Some(r#"{"chars":4}"#)).unwrap(),
        serde_json::json!({ "chars": 4 })
    );
    assert!(matches!(
        read_data(Some("not json")),
        Err(AgentSessionError::RecordInvalid(_))
    ));
    // A JSON value that is not an object is the record validator's
    // refusal, not the parser's.
    let parsed = read_data(Some("[1]")).unwrap();
    assert!(matches!(
        EmitRecord::new("stop", parsed),
        Err(AgentSessionError::RecordInvalid(_))
    ));
}

/// A `--type` outside the closed set must reach the verb (not die at
/// argv) so it is refused as the machine-readable `record_invalid`.
#[test]
fn an_unknown_record_type_is_a_record_invalid_refusal_not_a_usage_error() {
    let cli = crate::parse_cli([
        "phux",
        "agent",
        "emit",
        "@1",
        "--type",
        "not_a_type",
        "--json",
    ])
    .expect("an unknown --type must reach the verb, not die at argv");
    let Some(crate::commands::Command::Agent(crate::commands::AgentArgs {
        action:
            super::super::AgentAction::Emit {
                flags: super::super::action_flags::EmitFlags { event_type, .. },
                ..
            },
    })) = cli.command
    else {
        panic!("`agent emit` must parse to the emit action");
    };
    assert_eq!(event_type, "not_a_type", "argv carries the word verbatim");

    assert!(
        matches!(
            EmitRecord::new(&event_type, serde_json::json!({})),
            Err(AgentSessionError::RecordInvalid(_))
        ),
        "the closed set is enforced by the record validator",
    );
}

/// Every server-side failure maps onto its exit; a transport failure keeps
/// the shared no-server family.
#[test]
fn every_library_error_lands_on_its_registered_code_and_exit() {
    let socket = Path::new("/tmp/unused.sock");
    let at = ResourceId::local(7);
    for (err, code, exit) in [
        (
            AgentSessionError::Unsupported,
            json_err::codes::UNSUPPORTED_SERVER,
            2,
        ),
        (
            AgentSessionError::NoSession {
                terminal: at.clone(),
            },
            json_err::codes::NO_AGENT_SESSION,
            2,
        ),
        (
            AgentSessionError::AmbiguousSession {
                terminal: at.clone(),
                candidates: vec![ResourceId::local(9), ResourceId::local(10)],
            },
            json_err::codes::AGENT_SESSION_AMBIGUOUS,
            2,
        ),
        (
            AgentSessionError::WrongKind {
                resource: at.clone(),
                expected: resource::AGENT_SESSION,
                actual: resource::TERMINAL.to_owned(),
            },
            json_err::codes::WRONG_RESOURCE_KIND,
            2,
        ),
        (
            AgentSessionError::ParentNotFound { parent: at.clone() },
            json_err::codes::PARENT_NOT_FOUND,
            1,
        ),
        (
            AgentSessionError::ParentKindMismatch { parent: at.clone() },
            json_err::codes::PARENT_KIND_MISMATCH,
            2,
        ),
        (
            AgentSessionError::RecordInvalid("x".to_owned()),
            json_err::codes::RECORD_INVALID,
            2,
        ),
        (
            AgentSessionError::NotProducer {
                resource: at,
                message: "x".to_owned(),
            },
            json_err::codes::NOT_PRODUCER,
            2,
        ),
        (
            AgentSessionError::Overflow("x".to_owned()),
            json_err::codes::OVERFLOW,
            2,
        ),
        (
            AgentSessionError::Refused("x".to_owned()),
            json_err::codes::AGENT_SESSION_REFUSED,
            1,
        ),
    ] {
        let status = report_session_error(false, &err, socket, "agent emit");
        assert_eq!(status, ExitCode::from(exit), "{code}: {err}");
    }
    // A transport failure keeps the shared no-server family (exit 1).
    let err = AgentSessionError::Transport(AttachError::Disconnected);
    assert_eq!(
        report_session_error(false, &err, socket, "agent log"),
        ExitCode::from(1)
    );
}
