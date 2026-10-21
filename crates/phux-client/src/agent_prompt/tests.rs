#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
#![allow(
    clippy::future_not_send,
    reason = "the futures under test are !Send by design"
)]

use std::cell::{Cell, RefCell};

use tokio::net::UnixListener;

use crate::testkit::{EndOfScript, ScriptSpec, ScriptedServer};

use phux_protocol::wire::frame::{MAX_APPLY_INPUT_COMMAND_BODY, MAX_APPLY_INPUT_EVENTS};

use super::*;

fn op_id(fill: u8) -> InputOperationId {
    InputOperationId::new([fill.max(1); 16]).expect("non-zero operation id")
}

fn error(code: ErrorCode) -> CommandResult {
    CommandResult::Error {
        code,
        message: "scripted".to_owned(),
    }
}

/// The batch is `[Paste(trusted), Key(Enter)]`, Enter last; text that
/// reads like a key spec (`Enter`) is still pasted as text.
#[test]
fn a_prompt_is_one_trusted_paste_then_enter() {
    let events = prompt_events("Enter");
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(
        events[0],
        InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Trusted,
            data: b"Enter".to_vec(),
        })
    );
    assert!(
        matches!(events[1], InputEvent::Key(_)),
        "Enter must be last: {events:?}"
    );
}

#[test]
fn multiline_prompt_text_is_refused_before_any_round_trip() {
    assert_eq!(
        validate_prompt_text("do this\nthen that"),
        Err(Refusal::MultilineText { newlines: 1 })
    );
    assert_eq!(validate_prompt_text("").unwrap_err(), Refusal::EmptyText);
    assert_eq!(validate_prompt_text("one line"), Ok(()));
}

#[test]
fn the_inline_prompt_ceiling_binds_at_4096_bytes() {
    let at_limit = "x".repeat(MAX_PROMPT_BYTES);
    assert_eq!(validate_prompt_text(&at_limit), Ok(()));

    let over = "x".repeat(MAX_PROMPT_BYTES + 1);
    assert_eq!(
        validate_prompt_text(&over),
        Err(Refusal::TooLarge {
            unit: "bytes",
            measured: MAX_PROMPT_BYTES + 1,
            limit: MAX_PROMPT_BYTES,
            wire: false,
        })
    );
}

/// Key-event text counts toward the body cap, as on the server: a batch
/// whose paste alone fits is refused locally once key text tips it over.
#[test]
fn key_text_counts_toward_the_wire_body_cap() {
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

    let batch = |key_text: Option<&str>| {
        // Header (283) plus two 32-byte per-event allowances.
        let paste_len = MAX_APPLY_INPUT_COMMAND_BODY - (1 + 16 + 264 + 2) - 2 * 32;
        vec![
            InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: vec![b'x'; paste_len],
            }),
            InputEvent::Key(KeyEvent {
                action: KeyAction::Press,
                key: PhysicalKey::A,
                mods: ModSet::empty(),
                consumed_mods: ModSet::empty(),
                composing: false,
                text: key_text.map(str::to_owned),
                unshifted_codepoint: None,
            }),
        ]
    };
    assert_eq!(validate_batch(&batch(None)), Ok(()));
    assert_eq!(
        validate_batch(&batch(Some("a"))),
        Err(Refusal::TooLarge {
            unit: "bytes",
            measured: MAX_APPLY_INPUT_COMMAND_BODY + 1,
            limit: MAX_APPLY_INPUT_COMMAND_BODY,
            wire: true,
        })
    );
}

/// Both wire caps are enforced client-side with `wire: true`.
#[test]
fn the_wire_caps_are_enforced_client_side_and_never_split() {
    assert_eq!(validate_batch(&prompt_events(&"x".repeat(4096))), Ok(()));

    let huge = prompt_events(&"x".repeat(MAX_APPLY_INPUT_COMMAND_BODY));
    match validate_batch(&huge) {
        Err(Refusal::TooLarge {
            unit,
            limit,
            wire: true,
            ..
        }) => {
            assert_eq!(unit, "bytes");
            assert_eq!(limit, MAX_APPLY_INPUT_COMMAND_BODY);
        }
        other => panic!("a 64 KiB payload must be refused on the wire cap: {other:?}"),
    }

    let many: Vec<InputEvent> = std::iter::repeat_n(
        InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Trusted,
            data: Vec::new(),
        }),
        MAX_APPLY_INPUT_EVENTS + 1,
    )
    .collect();
    assert_eq!(
        validate_batch(&many),
        Err(Refusal::TooLarge {
            unit: "events",
            measured: MAX_APPLY_INPUT_EVENTS + 1,
            limit: MAX_APPLY_INPUT_EVENTS,
            wire: true,
        })
    );
}

/// Every reply maps onto exactly one reading. Delivery-unknown, internal
/// errors, and unnameable codes pessimize to `Unknown`; only
/// `RESOURCE_EXHAUSTED` is `Busy`; `INPUT_NOT_WRITTEN` is neither.
#[test]
fn every_reply_has_exactly_one_reading() {
    type Case = (CommandResult, fn(&ApplyVerdict) -> bool);
    let cases: &[Case] = &[
        (CommandResult::Ok, |v| *v == ApplyVerdict::Acked),
        (error(ErrorCode::InputDeliveryUnknown), |v| {
            matches!(v, ApplyVerdict::Unknown(_))
        }),
        (error(ErrorCode::InternalError), |v| {
            matches!(v, ApplyVerdict::Unknown(_))
        }),
        (error(ErrorCode::NotAttached), |v| {
            matches!(v, ApplyVerdict::Unknown(_))
        }),
        (error(ErrorCode::InputNotWritten), |v| {
            matches!(v, ApplyVerdict::NotWritten(_))
        }),
        (error(ErrorCode::ResourceExhausted), |v| {
            matches!(v, ApplyVerdict::Busy(_))
        }),
        (error(ErrorCode::TerminalNotFound), |v| {
            matches!(v, ApplyVerdict::NotFound(_))
        }),
        (error(ErrorCode::InputLeaseHeld), |v| {
            matches!(v, ApplyVerdict::Refused(Refusal::InputLeaseHeld(_)))
        }),
        (error(ErrorCode::CanonicalLimitExceeded), |v| {
            matches!(v, ApplyVerdict::Refused(Refusal::CanonicalLimitExceeded(_)))
        }),
        (error(ErrorCode::UnsafePaste), |v| {
            matches!(v, ApplyVerdict::Refused(Refusal::UnsafePaste(_)))
        }),
        (error(ErrorCode::InvalidCommand), |v| {
            matches!(v, ApplyVerdict::Refused(Refusal::InvalidBatch(_)))
        }),
        (error(ErrorCode::PermissionDenied), |v| {
            matches!(v, ApplyVerdict::Refused(Refusal::PermissionDenied(_)))
        }),
        (error(ErrorCode::UnsupportedSatelliteRoute), |v| {
            matches!(v, ApplyVerdict::Refused(Refusal::SatelliteTarget { .. }))
        }),
    ];
    for (result, check) in cases {
        let verdict = classify(result);
        assert!(check(&verdict), "{result:?} read as {verdict:?}");
    }
}

/// A retry reuses the operation id: a fresh one is exactly the duplicate
/// `APPLY_INPUT` exists to prevent.
#[tokio::test(start_paused = true)]
async fn a_retry_reuses_the_operation_id_and_never_mints_a_new_one() {
    let id = op_id(0x5a);
    let seen: RefCell<Vec<InputOperationId>> = RefCell::new(Vec::new());
    let (verdict, attempts) = submit_with_backoff(id, &backoff_schedule(&id), |attempt_id| {
        seen.borrow_mut().push(attempt_id);
        let count = seen.borrow().len();
        async move {
            Ok::<_, AttachError>(if count < 3 {
                ApplyVerdict::Busy("lane busy".to_owned())
            } else {
                ApplyVerdict::Acked
            })
        }
    })
    .await
    .expect("the fake never fails");

    assert_eq!(verdict, ApplyVerdict::Acked);
    assert_eq!(attempts, 3);
    let seen = seen.into_inner();
    assert_eq!(seen.len(), 3);
    assert!(
        seen.iter().all(|observed| *observed == id),
        "every attempt must carry the id generated once for this invocation"
    );
}

/// A non-`Busy` verdict, in particular an unknown, is never retried.
#[tokio::test(start_paused = true)]
async fn an_unknown_result_is_submitted_exactly_once() {
    let id = op_id(0x11);
    let calls = Cell::new(0_u32);
    let (verdict, attempts) = submit_with_backoff(id, &backoff_schedule(&id), |_| {
        calls.set(calls.get() + 1);
        async { Ok::<_, AttachError>(ApplyVerdict::Unknown("delivery unknown".to_owned())) }
    })
    .await
    .expect("the fake never fails");

    assert!(matches!(verdict, ApplyVerdict::Unknown(_)));
    assert_eq!(attempts, 1);
    assert_eq!(calls.get(), 1);
}

/// The backoff budget outlives the server's 5 s admission hold at every
/// jitter draw, and jitter decorrelates distinct ids.
#[test]
fn the_backoff_floor_outlasts_the_servers_completion_wait() {
    let floor: Duration = BACKOFF_STEPS
        .iter()
        .map(|step| *step * u32::try_from(1_000 - JITTER_PERMILLE).unwrap_or(1) / 1_000)
        .sum();
    assert!(
        floor > Duration::from_secs(6),
        "the worst-case backoff budget {floor:?} must exceed the ~5s admission hold"
    );
    assert!(
        SUBMIT_DEADLINE > Duration::from_secs(5),
        "the submit deadline must outlast the server's own completion wait"
    );
    for fill in [0x01, 0x7f, 0xfe, 0xff] {
        let schedule = backoff_schedule(&op_id(fill));
        let total: Duration = schedule.iter().sum();
        assert!(total >= floor, "{fill:#x} produced {total:?}");
        for (jittered, base) in schedule.iter().zip(BACKOFF_STEPS) {
            assert!(*jittered >= base.mul_f64(0.849), "{jittered:?} vs {base:?}");
            assert!(*jittered <= base.mul_f64(1.151), "{jittered:?} vs {base:?}");
        }
    }
    assert_ne!(
        backoff_schedule(&op_id(0x01)),
        backoff_schedule(&op_id(0xfe))
    );
}

/// A satellite target is refused before a socket is opened (the path
/// cannot exist), never downgraded to `ROUTE_INPUT`.
#[tokio::test]
async fn a_satellite_target_is_refused_rather_than_downgraded() {
    let outcome = prompt_agent(
        Path::new("/nonexistent/phux-must-not-connect.sock"),
        &ResourceId::satellite("devbox", 3),
        "ship it",
        op_id(0x22),
        &|_record| None,
        None,
    )
    .await;
    match outcome {
        Err(PromptError::Refused(Refusal::SatelliteTarget { host })) => {
            assert_eq!(host, "devbox");
        }
        other => panic!("a satellite target must be refused: {other:?}"),
    }
}

/// A server without `ACKNOWLEDGED_INPUT` (the scripted server advertises
/// no features) is refused, not downgraded. The rest of the wire contract
/// lives in `tests/connection/agent_prompt_wire.rs`.
#[tokio::test]
async fn a_server_without_acknowledged_input_is_refused_not_downgraded() {
    let dir = tempfile::tempdir().expect("temp dir");
    let socket = dir.path().join("phux.sock");
    let listener = UnixListener::bind(&socket).expect("bind scripted server");
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let spec = ScriptSpec::new()
                .metadata(|_scope, key| {
                    (key == RESOURCE_AGENT_KEY).then(|| {
                        br#"{"name":"reviewer","kind":"claude","state":"working"}"#.to_vec()
                    })
                })
                .end(EndOfScript::ServeUntilDetach);
            tokio::spawn(async move {
                ScriptedServer::on_stream(stream, spec).run().await;
            });
        }
    });

    let outcome = prompt_agent(
        &socket,
        &ResourceId::local(7),
        "ship it",
        op_id(0x33),
        &|_record| None,
        None,
    )
    .await;
    assert!(
        matches!(
            outcome,
            Err(PromptError::Refused(Refusal::NoAcknowledgedInput))
        ),
        "an older server must be refused rather than downgraded: {outcome:?}"
    );
}
