#![allow(clippy::expect_used, reason = "tests")]

use phux_protocol::input::paste::PasteTrust;

use super::*;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// The grammar this verb binds is the one reserved for the proposed
/// `%name` selector.
#[test]
fn bound_names_are_addressable_names() {
    for name in ["build", "a", "rev-2", "with_underscore", &"x".repeat(32)] {
        assert!(is_addressable_name(name), "{name} must be addressable");
    }
    for name in ["", "Build", "2fast", "-lead", "has space", &"x".repeat(33)] {
        assert!(!is_addressable_name(name), "{name} must be refused");
    }
}

/// Env rides `env NAME=value` (a `VAR=v cmd` prefix is not valid fish);
/// control characters, bad env names, empty and oversized lines are refused.
#[test]
fn shell_line_quotes_every_word_and_refuses_what_quoting_cannot_contain() {
    let env = |key: &str, value: &str| BTreeMap::from([(key.to_owned(), value.to_owned())]);
    assert_eq!(
        shell_line(
            &argv(&["/abs/claude", "--resume", "abc-123"]),
            &env("PHUX_CLAUDE_SESSION_ID", "abc-123")
        ),
        Ok("env 'PHUX_CLAUDE_SESSION_ID=abc-123' '/abs/claude' '--resume' 'abc-123'".to_owned())
    );
    assert_eq!(
        shell_line(&argv(&["codex"]), &BTreeMap::new()),
        Ok("'codex'".to_owned())
    );
    assert_eq!(
        shell_line(&argv(&["claude", "--flag\nrm -rf /"]), &BTreeMap::new()),
        Err(ShellLineError::Control {
            index: 1,
            element: "--flag\nrm -rf /".to_owned(),
        })
    );
    assert!(shell_line(&argv(&["cl\u{0}aude"]), &BTreeMap::new()).is_err());
    assert_eq!(
        shell_line(&[], &BTreeMap::new()),
        Err(ShellLineError::Empty)
    );
    assert!(matches!(
        shell_line(
            &argv(&["claude", &"x".repeat(MAX_SHELL_LINE)]),
            &BTreeMap::new()
        ),
        Err(ShellLineError::TooLong { .. })
    ));
    assert_eq!(
        shell_line(&argv(&["claude"]), &env("NOT AN IDENT", "x")),
        Err(ShellLineError::EnvName("NOT AN IDENT".to_owned()))
    );
}

/// The kind comparison is what turns "something started" into "the thing
/// I asked for started". An absent kind is reported, not failed: an
/// explicit hook writer may own the record and stand the detector down.
/// The typed line exports the delegated socket through `env`, quoted like
/// every other word, and a server without delegation types the plan's.
#[test]
fn the_typed_line_points_the_agent_at_its_delegated_socket() {
    let plan = Plan {
        kind: "codex".to_owned(),
        line: "'codex' '--x'".to_owned(),
        argv: argv(&["codex", "--x"]),
        env: BTreeMap::from([("A".to_owned(), "1".to_owned())]),
        resolved_cwd: PathBuf::from("/"),
        integration_id: "codex".to_owned(),
        timeout: Duration::from_secs(1),
    };
    let delegation = |socket: &str| {
        phux_client::delegation::Delegation::new(
            "delegated:1".to_owned(),
            phux_protocol::ids::SessionKey::new([1; 16]).expect("non-zero"),
            socket.to_owned(),
            0,
        )
    };
    assert_eq!(
        delegated_line(&plan, Some(&delegation("/tmp/phux d/agent's.sock"))).unwrap(),
        r"env 'A=1' 'PHUX_SOCKET=/tmp/phux d/agent'\''s.sock' 'codex' '--x'"
    );
    assert_eq!(delegated_line(&plan, None).unwrap(), plan.line);
    assert!(delegated_line(&plan, Some(&delegation("/tmp/a\nb"))).is_err());
}

#[test]
fn kind_verdict_separates_confirmed_absent_and_wrong() {
    let with = |kind: Option<&str>| AgentRecord {
        name: "build".to_owned(),
        kind: kind.map(str::to_owned),
        ..AgentRecord::default()
    };
    assert_eq!(
        kind_verdict(Some(&with(Some("claude"))), "claude"),
        KindVerdict::Confirmed
    );
    assert_eq!(
        kind_verdict(Some(&with(Some("CLAUDE"))), " claude "),
        KindVerdict::Confirmed
    );
    assert_eq!(
        kind_verdict(Some(&with(None)), "claude"),
        KindVerdict::Unconfirmed
    );
    assert_eq!(kind_verdict(None, "claude"), KindVerdict::Unconfirmed);
    assert_eq!(
        kind_verdict(Some(&with(Some("codex"))), "claude"),
        KindVerdict::Mismatch
    );
}

/// The submitted batch is exactly ADR-0076 point 3's shape: one trusted
/// paste carrying the whole line, then the real Enter key, in that order.
/// Enter last means a partial delivery loses the submission rather than
/// truncating the command.
#[test]
fn the_submit_batch_is_one_trusted_paste_then_enter() {
    let events = submit_events("'claude' '--resume' 'x'");
    assert_eq!(events.len(), 2, "{events:?}");
    match &events[0] {
        InputEvent::Paste(paste) => {
            assert_eq!(paste.trust, PasteTrust::Trusted);
            assert_eq!(paste.data, b"'claude' '--resume' 'x'");
        }
        other => panic!("first event must be the paste, got {other:?}"),
    }
    assert!(
        matches!(events[1], InputEvent::Key(_)),
        "second event must be Enter, got {:?}",
        events[1]
    );
}

/// The shared verdicts retain their proof boundary when projected onto
/// start-specific errors. `NotWritten` must roll the provisional name
/// back; `Unknown` must retain it.
#[test]
fn submit_verdicts_map_to_their_unwind_safety_and_exit() {
    let unknown = submit_verdict(ApplyVerdict::Unknown("lost".to_owned()));
    assert!(!unknown.wrote_nothing);
    assert_eq!(unknown.refusal.err.code, codes::AGENT_START_UNKNOWN);
    // NOT exit 3: 3 means "retry is correct", and retrying this under a
    // new id produces the duplicate the acknowledged lane prevents.
    assert_eq!(unknown.refusal.exit, EXIT_FAILURE);

    for verdict in [
        ApplyVerdict::Busy("no".to_owned()),
        ApplyVerdict::NotWritten("no PTY".to_owned()),
        ApplyVerdict::NotFound("gone".to_owned()),
        ApplyVerdict::Refused(ApplyRefusal::InputLeaseHeld("no".to_owned())),
        ApplyVerdict::Refused(ApplyRefusal::CanonicalLimitExceeded("no".to_owned())),
    ] {
        let failure = submit_verdict(verdict);
        assert!(
            failure.wrote_nothing,
            "a proven non-delivery must be unwindable: {failure:?}"
        );
    }
    assert_eq!(
        submit_verdict(ApplyVerdict::Refused(ApplyRefusal::InputLeaseHeld(
            "no".to_owned()
        )))
        .refusal
        .exit,
        EXIT_USAGE
    );
    assert_eq!(
        submit_verdict(ApplyVerdict::Busy("no".to_owned()))
            .refusal
            .exit,
        EXIT_FAILURE
    );
}

fn request<'a>(name: &'a str, kind: &'a str) -> StartRequest<'a> {
    StartRequest {
        name,
        kind,
        target: "@7",
        integration: None,
        timeout: None,
        no_wait: false,
        force: false,
        json: false,
        args: &[],
    }
}

/// A bad name is refused before the kind is resolved, and a kind with no
/// manifest is refused before anything is typed, naming a way forward.
#[test]
fn an_unmanifested_kind_is_refused_before_any_write() {
    let refusal = preflight(&request("Build Bot", "no-such-agent-kind"))
        .expect_err("a bad name must be refused");
    assert_eq!(refusal.err.code, codes::INVALID_AGENT_NAME);

    let refusal = preflight(&request("build", "no-such-agent-kind"))
        .expect_err("an unmanifested kind must be refused");
    assert_eq!(refusal.err.code, codes::UNSUPPORTED_AGENT_KIND);
    assert_eq!(refusal.exit, EXIT_USAGE);
    assert!(
        refusal.err.remedy.contains("--no-wait"),
        "the refusal must not be a dead end: {}",
        refusal.err.remedy
    );
}

/// An unclaimed `--kind CLAUDE` falls back to the integration id of the
/// canonical kind (`claude`), never the string as typed.
#[test]
fn an_unclaimed_kind_falls_back_to_the_canonical_kind_not_the_typed_one() {
    if agent_explain::resolve_kind("CLAUDE").as_deref() != Some("claude") {
        // No shipped manifest to canonicalize through (PHUX_AGENT_DETECT=0);
        // `the_shipped_manifests_resolve_as_kinds` covers that separately.
        return;
    }
    match preflight(&request("canonical-kind", "CLAUDE")) {
        Ok(plan) => {
            assert_eq!(plan.kind, "claude");
            assert!(
                !plan.integration_id.contains("CLAUDE"),
                "the resolved id must come from the canonical kind: {}",
                plan.integration_id
            );
        }
        Err(refusal) => assert!(
            !refusal.err.message.contains("CLAUDE"),
            "the refusal must name the canonical id, not the typed kind: {}",
            refusal.err.message
        ),
    }
}

/// Every shipped detection manifest is a legitimate `--kind`.
/// This is the roster the refusal above prints.
#[test]
fn the_shipped_manifests_resolve_as_kinds() {
    let kinds = agent_explain::kinds();
    for kind in [
        "claude",
        "codex",
        "opencode",
        "pi",
        "omp",
        "grok",
        "amp",
        "cursor-agent",
        "gemini",
        "goose",
        "aider",
    ] {
        assert!(
            kinds.iter().any(|loaded| loaded == kind),
            "{kind} must ship a detection manifest; loaded: {kinds:?}"
        );
        assert_eq!(agent_explain::resolve_kind(kind).as_deref(), Some(kind));
    }
}
