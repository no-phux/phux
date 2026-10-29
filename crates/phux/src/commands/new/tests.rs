//! Unit tests for `phux new`.

use super::empty::{create_empty_session_via_metadata, empty_session_json};
use super::naming::{
    RANDOM_NAME_ATTEMPTS, choose_session_name, fresh_session_name, requested_session_name,
    unique_session_name,
};
use super::{AttachTarget, ServerTarget, new_session_json, new_session_target, seed_cwd};
use std::path::{Path, PathBuf};

use phux_config::{NameRng, random_name};

/// ADR-0105: `phux new --empty --json` keeps the three documented keys, with
/// `terminal_id` null, and adds `empty` and `keep_empty`.
#[test]
fn empty_session_json_pins_the_contract_shape() {
    let doc = empty_session_json("parked");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["session"], "parked");
    assert!(doc["terminal_id"].is_null());
    assert_eq!(doc["empty"], true);
    assert_eq!(doc["keep_empty"], true);
    assert_eq!(doc.as_object().map(serde_json::Map::len), Some(5));
}

/// Against a server without `KeepEmptySessions`, `phux new --empty` refuses
/// on the capability check alone, with no `GET_STATE`.
#[tokio::test]
async fn create_empty_session_refuses_before_any_get_state_when_unsupported() {
    use phux_client::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::wire::frame::FrameKind;

    let dir = tempfile::tempdir().expect("temp dir");
    let socket = dir.path().join("phux.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
    // A bare `ScriptSpec` advertises no `KeepEmptySessions` bit.
    let spec = ScriptSpec::new();
    let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

    let target = ServerTarget::local(&socket);
    let result = create_empty_session_via_metadata(&target, "parked", false).await;
    let seen = server.await.expect("scripted server task");

    assert_eq!(result, Err(std::process::ExitCode::FAILURE));
    assert_eq!(
        seen.len(),
        1,
        "expected only the HELLO handshake, no GET_STATE; sent {seen:?}"
    );
    assert!(matches!(seen[0], FrameKind::Hello { .. }));
}

const CWD: &str = "/home/me/proj";

/// The first `count` names a generator seeded with `seed` will produce.
fn picks(seed: u64, count: usize) -> Vec<String> {
    let mut rng = NameRng::seeded(seed);
    (0..count).map(|_| random_name(&mut rng)).collect()
}

/// A free generated name is taken as-is on the first draw.
#[test]
fn fresh_session_name_takes_a_free_random_pick() {
    let expected = picks(4, 1).remove(0);
    let name = fresh_session_name(
        &["other".to_owned()],
        "${random-name}",
        Path::new(CWD),
        &mut NameRng::seeded(4),
    );
    assert_eq!(name, expected);
}

/// A taken pick is retried with the next draw rather than suffixed.
#[test]
fn fresh_session_name_retries_a_taken_random_pick() {
    let sequence = picks(4, 2);
    assert_ne!(sequence[0], sequence[1], "seed must yield distinct picks");
    let name = fresh_session_name(
        &[sequence[0].clone()],
        "${random-name}",
        Path::new(CWD),
        &mut NameRng::seeded(4),
    );
    assert_eq!(name, sequence[1]);
}

#[test]
fn a_suffixed_name_stays_within_the_byte_limit() {
    let base = "é".repeat(128);
    let name = unique_session_name(std::slice::from_ref(&base), &base);
    assert!(
        name.len() <= phux_client::rename::MAX_SESSION_NAME_BYTES,
        "{}",
        name.len()
    );
    assert!(name.ends_with("-2"));
}

/// Every attempt taken ⇒ the first pick gets the numeric suffix, so
/// uniqueness holds however crowded the name space is.
#[test]
fn fresh_session_name_falls_back_to_a_numeric_suffix() {
    let existing = picks(4, RANDOM_NAME_ATTEMPTS);
    let name = fresh_session_name(
        &existing,
        "${random-name}",
        Path::new(CWD),
        &mut NameRng::seeded(4),
    );
    assert_eq!(name, format!("{}-2", existing[0]));
    assert!(!existing.contains(&name));
}

/// A generated name obeys the rule an explicit one does: `phux new` run
/// in a directory named `@proj` made a session `@proj`, which every
/// selector reads as a pane id.
#[test]
fn fresh_session_name_is_addressable_whatever_the_directory() {
    let mut rng = NameRng::seeded(4);
    for (dir, template, expected) in [
        ("/work/@proj", "${cwd-basename}", "_proj"),
        ("/work/#notes", "${cwd-basename}", "_notes"),
        ("/work/a:b", "${cwd-basename}", "a_b"),
        ("/work/proj", "main:${cwd-basename}", "main_proj"),
    ] {
        let name = fresh_session_name(&[], template, Path::new(dir), &mut rng);
        assert_eq!(name, expected, "{dir} {template}");
        assert_eq!(phux_client::rename::check_session_name(&name), Ok(()));
    }
}

/// A deterministic template keeps the historical `base`, `base-2` shape.
#[test]
fn fresh_session_name_suffixes_a_deterministic_template() {
    let mut rng = NameRng::seeded(4);
    assert_eq!(
        fresh_session_name(&[], "default", Path::new(CWD), &mut rng),
        "default"
    );
    assert_eq!(
        fresh_session_name(
            &["proj".to_owned()],
            "${cwd-basename}",
            Path::new(CWD),
            &mut rng
        ),
        "proj-2"
    );
}

/// `phux new --json` pins `schema_version` 1 plus the two documented
/// fields — the shape `docs/scripting.md#creating-panes-and-sessions` promises.
#[test]
fn new_session_json_pins_the_contract_shape() {
    let doc = new_session_json("work", 2);
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["session"], "work");
    assert_eq!(doc["terminal_id"], 2);
    assert_eq!(doc.as_object().map(serde_json::Map::len), Some(3));
}

/// ADR-0160: a session the user names is persistent; a generated name
/// is ephemeral; `--ephemeral` overrides a name; `--empty` is always
/// persistent, since an ephemeral session is never empty.
#[test]
fn named_sessions_are_persistent_and_generated_ones_ephemeral() {
    use phux_protocol::wire::frame::SessionLifetime::{Ephemeral, Persistent};
    assert_eq!(super::session_lifetime(true, false, false), Persistent);
    assert_eq!(super::session_lifetime(false, false, false), Ephemeral);
    assert_eq!(super::session_lifetime(true, false, true), Ephemeral);
    assert_eq!(super::session_lifetime(false, true, false), Persistent);
    assert_eq!(super::session_lifetime(true, true, false), Persistent);
}

/// phux-0db: `phux new` without `--cwd` seeds a local session in the
/// *client's* cwd, not `None` (= the daemon's CWD).
#[test]
fn seed_cwd_defaults_a_local_session_to_the_client_cwd() {
    let expected = std::env::current_dir()
        .expect("test cwd")
        .to_string_lossy()
        .into_owned();
    assert_eq!(seed_cwd(None, false), Some(expected));
}

/// A remote server gets no client-cwd default (a local path names
/// nothing there), but an explicit `--cwd` is still sent verbatim.
#[test]
fn seed_cwd_sends_only_an_explicit_cwd_to_a_remote() {
    assert_eq!(seed_cwd(None, true), None);
    assert_eq!(
        seed_cwd(Some(PathBuf::from("/srv/work")), true),
        Some("/srv/work".to_owned())
    );
}

/// An explicit `--cwd` wins over the client-cwd default, and a
/// non-empty command rides along unchanged.
#[test]
fn new_session_target_honors_explicit_cwd_and_command() {
    assert_eq!(
        new_session_target(
            "proj".to_owned(),
            vec!["vim".to_owned(), "notes.txt".to_owned()],
            seed_cwd(Some(PathBuf::from("/somewhere/else")), false),
        ),
        AttachTarget::CreateIfMissing {
            name: "proj".to_owned(),
            command: Some(vec!["vim".to_owned(), "notes.txt".to_owned()]),
            cwd: Some("/somewhere/else".to_owned()),
        }
    );
}

/// The positional and `-s` spellings agree or conflict; they never
/// silently pick one.
#[test]
fn requested_session_name_merges_the_two_spellings() {
    assert_eq!(
        requested_session_name(Some("a".to_owned()), None).ok(),
        Some(Some("a".to_owned()))
    );
    assert_eq!(
        requested_session_name(None, Some("b".to_owned())).ok(),
        Some(Some("b".to_owned()))
    );
    assert_eq!(
        requested_session_name(Some("a".to_owned()), Some("a".to_owned())).ok(),
        Some(Some("a".to_owned()))
    );
    assert!(requested_session_name(Some("a".to_owned()), Some("b".to_owned())).is_err());
    assert_eq!(requested_session_name(None, None).ok(), Some(None));
}

/// A taken name is refused; a free one is kept.
#[test]
fn choose_session_name_refuses_a_duplicate() {
    let existing = vec!["work".to_owned()];
    assert!(choose_session_name(Some("work".to_owned()), &existing).is_err());
    assert_eq!(
        choose_session_name(Some("play".to_owned()), &existing).ok(),
        Some("play".to_owned())
    );
}

#[test]
fn unique_session_name_uses_the_base_then_numeric_suffixes() {
    // Free base ⇒ the base verbatim (no "-2" churn, no bare "0").
    assert_eq!(unique_session_name(&[], "default"), "default");
    assert_eq!(
        unique_session_name(&["other".to_owned()], "default"),
        "default",
    );
    // Base taken ⇒ first free `base-N`, starting at 2.
    assert_eq!(
        unique_session_name(&["default".to_owned()], "default"),
        "default-2",
    );
    assert_eq!(
        unique_session_name(&["default".to_owned(), "default-2".to_owned()], "default",),
        "default-3",
    );
    // A non-default template base works the same way.
    assert_eq!(unique_session_name(&["phux".to_owned()], "phux"), "phux-2");
}
