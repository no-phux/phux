//! Unit tests for the host registry rows, lookups, edits, and rendering.

use std::path::PathBuf;

use super::HostRole;
use super::list::{HostRow, auth_display, empty_state, render_table, sort_rows};
use super::manage::{
    edit_host_field_at, inbound_rename_refusal, resolve_host_role, resolve_rm_role,
};
use crate::commands::json_err::codes;

pub(super) fn row(name: &str, role: HostRole) -> HostRow {
    HostRow {
        name: name.to_owned(),
        role,
        endpoint: "ssh://example".to_owned(),
        enabled: (role == HostRole::Satellite).then_some(true),
        token_file: None,
        cert_fingerprint: None,
        tls_server_name: None,
        session: None,
        ssh: None,
        direct: None,
        client_cert: None,
        client_key: None,
    }
}

#[test]
fn host_edits_preserve_other_fields_and_refuse_inherited_or_colliding_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let original = "# keep this\n[[remote]]\nname = 'mini'\nendpoint = 'quic://example:8788'\ntoken-file = '/some/token'\n[[remote]]\nname = 'other'\nendpoint = 'ssh://other'\n[[satellites]]\nname = 'edge'\nendpoint = 'ssh://example'\nenabled = true\n";
    std::fs::write(&path, original).expect("seed");
    let mut mini = row("mini", HostRole::Remote);
    mini.endpoint = "quic://example:8788".to_owned();
    assert!(edit_host_field_at(&path, &mini, "name", "other".into()).is_err());
    assert_eq!(std::fs::read_to_string(&path).expect("unchanged"), original);
    edit_host_field_at(&path, &mini, "name", "desk".into()).expect("rename");
    edit_host_field_at(
        &path,
        &row("edge", HostRole::Satellite),
        "enabled",
        false.into(),
    )
    .expect("disable");
    let changed = std::fs::read_to_string(&path).expect("read");
    assert!(changed.starts_with("# keep this"));
    assert!(changed.contains("token-file = '/some/token'"));
    assert!(
        changed.contains("ssh = \"mini\""),
        "old repair destination survives rename: {changed}"
    );
    assert!(changed.contains("enabled = false"));
    assert!(
        edit_host_field_at(
            &path,
            &row("missing", HostRole::Remote),
            "name",
            "new".into()
        )
        .is_err()
    );
}

#[test]
fn non_destructive_host_lookup_still_refuses_ambiguous_names() {
    let (err, code) = resolve_host_role("mini", None, true, true, "rename").expect_err("ambiguous");
    assert_eq!(code, 2);
    assert!(err.remedy.contains("host rename --role remote mini"));
}

/// `host rm` with no `--role`: unique names resolve, a doubly-registered
/// name is refused (exit 2) with both disambiguating commands named, and
/// a miss is exit 1.
#[test]
fn rm_resolves_across_registries_and_refuses_ambiguity() {
    assert_eq!(
        resolve_rm_role("mini", None, true, false).map_err(|_| ()),
        Ok(HostRole::Remote)
    );
    assert_eq!(
        resolve_rm_role("edge", None, false, true).map_err(|_| ()),
        Ok(HostRole::Satellite)
    );

    let (err, exit_code) = resolve_rm_role("both", None, true, true)
        .expect_err("a doubly-registered name must be refused");
    assert_eq!(exit_code, 2, "ambiguity is a refusal, not a miss");
    assert!(
        err.remedy.contains("--role remote both") && err.remedy.contains("--role satellite both"),
        "the remedy names both disambiguating commands: {}",
        err.remedy
    );

    let (err, exit_code) = resolve_rm_role("ghost", None, false, false).expect_err("a miss errors");
    assert_eq!(exit_code, 1);
    assert!(err.remedy.contains("phux host ls"), "got {}", err.remedy);
}

/// `host rm --role X NAME` where the name lives in the OTHER registry:
/// the error names the role that would have matched instead of reading
/// as a plain miss.
#[test]
fn rm_wrong_role_remedy_names_the_matching_role() {
    let (err, exit_code) = resolve_rm_role("edge", Some(HostRole::Remote), false, true)
        .expect_err("wrong role is a miss with a teaching remedy");
    assert_eq!(exit_code, 1);
    assert!(
        err.remedy.contains("--role satellite edge"),
        "got {}",
        err.remedy
    );

    let (err, _) = resolve_rm_role("mini", Some(HostRole::Satellite), true, false)
        .expect_err("symmetric case");
    assert!(
        err.remedy.contains("--role remote mini"),
        "got {}",
        err.remedy
    );

    // The requested role matching wins even when both registries hold
    // the name — an explicit `--role` is never ambiguous.
    assert_eq!(
        resolve_rm_role("both", Some(HostRole::Satellite), true, true).map_err(|_| ()),
        Ok(HostRole::Satellite)
    );
}

/// Merged ordering is by name, remotes before satellites on a shared
/// name — independent of config-file entry order.
#[test]
fn merged_rows_sort_by_name_then_remote_first() {
    let mut rows = vec![
        row("zeta", HostRole::Satellite),
        row("alpha", HostRole::Satellite),
        row("mini", HostRole::Remote),
        row("alpha", HostRole::Remote),
    ];
    sort_rows(&mut rows);
    let order: Vec<_> = rows
        .iter()
        .map(|row| (row.name.as_str(), row.role))
        .collect();
    assert_eq!(
        order,
        vec![
            ("alpha", HostRole::Remote),
            ("alpha", HostRole::Satellite),
            ("mini", HostRole::Remote),
            ("zeta", HostRole::Satellite),
        ]
    );
}

/// The AUTH column classifies the transport: ssh rides ssh trust, the
/// paired transports report which halves of token + pin are on file.
#[test]
fn auth_display_classifies_transports() {
    assert_eq!(auth_display("ssh://mini", false, false), "ssh");
    // ssh needs no pairing even if material is (pointlessly) on file.
    assert_eq!(auth_display("  ssh://mini  ", true, true), "ssh");
    assert_eq!(auth_display("quic://mini:8788", true, true), "token+pin");
    assert_eq!(auth_display("wss://mini:8787", false, true), "pin");
    assert_eq!(auth_display("quic://mini:8788", true, false), "token");
    assert_eq!(auth_display("wss://mini:8787", false, false), "none");
    assert_eq!(auth_display("inbound://", false, false), "cert");
}

/// An inbound satellite's credential is bound to its name, so `rename`
/// refuses it; dialed satellites and remotes still rename.
#[test]
fn rename_refuses_only_inbound_satellites() {
    let mut inbound = row("sandbox", HostRole::Satellite);
    inbound.endpoint = "inbound://".to_owned();
    let refused = inbound_rename_refusal(&inbound).expect("inbound refused");
    assert_eq!(refused.code, codes::USAGE_ERROR);
    assert!(
        refused.remedy.contains("phux host ticket"),
        "{}",
        refused.remedy
    );
    assert!(inbound_rename_refusal(&row("mini", HostRole::Satellite)).is_none());
    let mut remote = row("mini", HostRole::Remote);
    remote.endpoint = "inbound://".to_owned();
    assert!(inbound_rename_refusal(&remote).is_none());
}

/// The table carries the five pinned columns; STATE is `-` for remotes
/// and the enabled bit for satellites.
#[test]
fn table_renders_role_state_and_auth_columns() {
    let rows = vec![
        HostRow {
            name: "mini".to_owned(),
            role: HostRole::Remote,
            endpoint: "quic://mini:8788".to_owned(),
            enabled: None,
            token_file: Some(PathBuf::from("/tokens/mini")),
            cert_fingerprint: Some("ab".repeat(32)),
            tls_server_name: None,
            session: Some("work".to_owned()),
            ssh: Some("me@mini".to_owned()),
            direct: None,
            client_cert: None,
            client_key: None,
        },
        HostRow {
            name: "edge".to_owned(),
            role: HostRole::Satellite,
            endpoint: "ssh://edge".to_owned(),
            enabled: Some(false),
            token_file: None,
            cert_fingerprint: None,
            tls_server_name: None,
            session: None,
            ssh: None,
            direct: None,
            client_cert: None,
            client_key: None,
        },
    ];
    let table = render_table(&rows);
    let mut lines = table.lines();
    let header = lines.next().expect("header line");
    for column in ["NAME", "ROLE", "ENDPOINT", "STATE", "AUTH"] {
        assert!(header.contains(column), "header lost {column}: {header}");
    }
    let mini = lines.next().expect("first row");
    assert!(mini.contains("remote") && mini.contains("token+pin"));
    assert!(mini.contains(" - "), "remote STATE is the `-` placeholder");
    let edge = lines.next().expect("second row");
    assert!(edge.contains("satellite") && edge.contains("disabled") && edge.contains("ssh"));
}

/// The empty states teach the one command that fills the table — the
/// ssh form, with the role flag under a satellite filter.
#[test]
fn empty_states_name_the_add_command() {
    let merged = empty_state(None);
    assert!(merged.contains("phux host add me@HOST"));
    assert!(merged.contains("--role satellite"));

    let remotes = empty_state(Some(HostRole::Remote));
    assert!(remotes.contains("phux host add me@HOST"));
    assert!(!remotes.contains("--role satellite"));

    let satellites = empty_state(Some(HostRole::Satellite));
    assert!(satellites.contains("--role satellite"));
}
