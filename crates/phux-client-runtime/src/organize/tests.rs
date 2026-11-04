use phux_client_core::organize::CreatedSession;
use phux_protocol::ids::SessionKey;

use super::*;

#[test]
fn curl_config_escapes_values_so_nothing_starts_a_new_option() {
    let config = curl_config(
        "k\"ey\n",
        &serde_json::json!({"a": "b\\c\"d\nurl = \"https://evil\""}),
        JEV_TIMEOUT,
    );
    assert!(
        config.contains(r#"header = "Authorization: Bearer k\"ey""#),
        "{config}"
    );
    assert_eq!(config.lines().filter(|l| l.starts_with("url =")).count(), 1);
    assert_eq!(config.lines().count(), 7, "{config}");
}

#[test]
fn jev_replies_carry_their_status_and_never_echo_the_body() {
    let ok = read_jev_reply(b"{\"answers\":{}}\n200", b"", true).expect("ok");
    assert!(ok.get("answers").is_some());
    let refused = read_jev_reply(b"{\"error\":\"secret\"}\n401", b"", true).unwrap_err();
    assert_eq!(refused, PlanError::Jev("HTTP 401".into()));
    let failed = read_jev_reply(b"", b"curl: (6) no host\nmore", false).unwrap_err();
    assert_eq!(
        failed,
        PlanError::Jev("request failed: curl: (6) no host".into())
    );
}

#[test]
fn a_key_command_must_succeed_and_print_something() {
    let argv = |line: &str| vec!["sh".to_owned(), "-c".to_owned(), line.to_owned()];
    assert_eq!(
        key_from_command(&argv("printf ' k1 \\n'")).as_deref(),
        Ok("k1")
    );
    assert!(key_from_command(&argv("true")).is_err());
    assert_eq!(
        key_from_command(&argv("printf 'k2\\nurl: https://evil\\n'")).as_deref(),
        Ok("k2")
    );
    assert!(key_from_command(&argv("echo 'a b'")).is_err());
    assert!(key_from_command(&argv("echo k; exit 3")).is_err());
    assert!(key_from_command(&[]).is_err());
}

#[test]
fn errors_never_carry_the_key() {
    let err = redact(PlanError::Jev("Bearer sk-123 rejected".into()), "sk-123\n");
    assert_eq!(err, PlanError::Jev("Bearer *** rejected".into()));
}

#[test]
fn created_sessions_carry_a_whole_identity() {
    let attrs = creation_attrs(&NewSession {
        name: "phux-2".into(),
        project_id: "github.com/no-phux/phux".into(),
        project_name: "phux".into(),
        checkout: "/src/phux".into(),
    });
    let project = phux_core::project_identity::SessionProject::from_attrs(
        attrs.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    )
    .expect("identity");
    assert_eq!(project.name, "phux");
}

#[test]
fn offline_never_runs_the_key_command() {
    let config = phux_config_core::OrganizeCfg {
        jev_key_command: Some(vec!["false".to_owned()]),
        offline: true,
    };
    let (judge, why) = Judge::configured(&config, false);
    assert!(matches!(judge, Judge::Heuristic));
    assert_eq!(why, None);
}

#[test]
fn an_unknown_scope_is_refused_before_judging() {
    let options = Options {
        session: Some("nope".into()),
        judge: Judge::Heuristic,
        min_confidence: 0.6,
        jev_timeout: JEV_TIMEOUT,
    };
    assert_eq!(
        plan_or_fallback(&Facts::default(), &options).unwrap_err(),
        PlanError::NoSuchSession("nope".into())
    );
}

/// A journal survives a write and read, and clearing it twice is fine.
#[test]
fn journal_persists() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("organize").join("last.json");
    assert_eq!(load_journal_at(&path).expect("read"), None);
    let mut journal = Journal::new();
    journal.created.push(CreatedSession {
        key: SessionKey::new([1; 16]).expect("non-zero"),
        name: "phux".into(),
    });
    save_journal_at(&path, &journal).expect("save");
    assert_eq!(load_journal_at(&path).expect("read"), Some(journal));
    clear_journal_at(&path).expect("clear");
    clear_journal_at(&path).expect("clear twice");
}

/// Undo refuses a record from a newer format instead of acting on it.
#[test]
fn a_newer_journal_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("last.json");
    let mut journal = Journal::new();
    journal.schema_version = JOURNAL_SCHEMA + 1;
    save_journal_at(&path, &journal).expect("save");
    let err = load_journal_at(&path).expect_err("newer");
    assert!(err.to_string().contains("newer"), "{err}");
}
