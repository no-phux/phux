//! Unit tests for `phux host add` and enrollment registration.

use std::path::{Path, PathBuf};

use super::HostRole;
use super::add::{AddMode, add_failure_error, relay_route_authority, role_flag_mismatch};
use super::args::AddOpts;
use super::enrollment::{Route, finish_enroll_in, keeps_credentials};
use crate::commands::JsonOpt;
use crate::commands::enroll::EnrollFailure;
use crate::commands::json_err::codes;

pub(super) fn opts() -> AddOpts {
    AddOpts {
        role: HostRole::Remote,
        name: None,
        endpoint_override: None,
        quic_port: 8788,
        no_service: false,
        ssh_only: false,
        remote_phux: "phux".to_owned(),
        install: false,
        accept_new_host_key: false,
        token_file: None,
        cert_fingerprint: None,
        tls_server_name: None,
        ca_fingerprint: None,
        session: None,
        disabled: false,
        json: JsonOpt { json: false },
    }
}

/// ADR-0180: a manual relay route needs the server's CA, which is
/// canonicalized; a CA anywhere but beside a QUIC relay route is refused.
#[test]
fn a_manual_relay_route_requires_the_servers_authority() {
    let relay = "quic://relay.example:4433";
    let mut routed = opts();
    routed.tls_server_name = Some("mini-route".to_owned());
    let err = relay_route_authority(relay, &routed).expect_err("no CA");
    assert!(err.remedy.contains("--ca-fingerprint"), "{}", err.remedy);

    routed.ca_fingerprint = Some(format!("sha256:{}", "AB:".repeat(31) + "AB"));
    assert_eq!(
        relay_route_authority(relay, &routed).expect("pinned"),
        Some(format!("sha256:{}", "ab".repeat(32)))
    );
    routed.ca_fingerprint = Some("sha256:nope".to_owned());
    assert!(relay_route_authority(relay, &routed).is_err());

    let mut direct = opts();
    assert_eq!(relay_route_authority(relay, &direct).expect("direct"), None);
    direct.ca_fingerprint = Some(format!("sha256:{}", "ab".repeat(32)));
    assert!(
        relay_route_authority(relay, &direct).is_err(),
        "a CA without a route"
    );
}

/// A flag paired with the role it does not apply to is refused with a
/// remedy that names the way out; coherent pairings pass.
#[test]
fn role_flag_mismatch_names_the_remedy() {
    let err = role_flag_mismatch(HostRole::Satellite, true, false)
        .expect("--session under --role satellite must be refused");
    assert_eq!(err.code, codes::REGISTRY);
    assert!(err.message.contains("--session"), "got {}", err.message);
    assert!(
        err.message.contains("no arrival to attach"),
        "the message explains WHY: {}",
        err.message
    );
    assert!(err.remedy.contains("drop --session"), "got {}", err.remedy);

    let err = role_flag_mismatch(HostRole::Remote, false, true)
        .expect("--disabled under --role remote must be refused");
    assert!(err.message.contains("--disabled"), "got {}", err.message);
    assert!(
        err.remedy.contains("--role satellite"),
        "the remedy names the role that accepts the flag: {}",
        err.remedy
    );

    // The coherent pairings pass.
    assert!(role_flag_mismatch(HostRole::Remote, true, false).is_none());
    assert!(role_flag_mismatch(HostRole::Satellite, false, true).is_none());
    assert!(role_flag_mismatch(HostRole::Remote, false, false).is_none());
}

/// The two forms are told apart from the arguments alone: a `NAME
/// ENDPOINT` pair or a bare URI is manual, anything else is an ssh
/// destination.
#[test]
fn add_mode_is_decided_by_the_arguments() {
    assert_eq!(
        AddMode::classify("mini", Some("quic://mini:8788"), &opts()).expect("pair"),
        AddMode::Manual {
            name: "mini".to_owned(),
            endpoint: "quic://mini:8788".to_owned()
        }
    );
    assert_eq!(
        AddMode::classify("ssh://me@box", None, &opts()).expect("uri"),
        AddMode::Manual {
            name: "box".to_owned(),
            endpoint: "ssh://me@box".to_owned()
        }
    );
    let named = AddOpts {
        name: Some("lab".to_owned()),
        ..opts()
    };
    assert_eq!(
        AddMode::classify("wss://10.0.0.5:8787", None, &named).expect("uri + --name"),
        AddMode::Manual {
            name: "lab".to_owned(),
            endpoint: "wss://10.0.0.5:8787".to_owned()
        }
    );
    assert_eq!(
        AddMode::classify("me@mini", None, &opts()).expect("ssh"),
        AddMode::Ssh {
            target: "me@mini".to_owned()
        }
    );
}

/// A flag from the other form is refused and the form it belongs to is
/// named, rather than the flag being silently ignored.
#[test]
fn add_mode_refuses_flags_from_the_other_form() {
    let ssh_only = AddOpts {
        ssh_only: true,
        ..opts()
    };
    let err = AddMode::classify("mini", Some("ssh://mini"), &ssh_only)
        .expect_err("--ssh-only is an ssh-form flag");
    assert!(err.message.contains("--ssh-only"), "got {}", err.message);
    assert!(
        err.remedy.contains("phux host add HOST"),
        "got {}",
        err.remedy
    );
    for (flag, opts) in [
        (
            "--install",
            AddOpts {
                install: true,
                ..opts()
            },
        ),
        (
            "--accept-new-host-key",
            AddOpts {
                accept_new_host_key: true,
                ..opts()
            },
        ),
    ] {
        let err = AddMode::classify("quic://mini:8788", None, &opts)
            .expect_err("an ssh-form flag on the manual form");
        assert!(err.message.contains(flag), "{flag}: {}", err.message);
    }

    let credentials = AddOpts {
        cert_fingerprint: Some("ab".repeat(32)),
        ..opts()
    };
    let err = AddMode::classify("me@mini", None, &credentials)
        .expect_err("credentials belong to the manual form");
    assert!(
        err.message.contains("--cert-fingerprint") && err.remedy.contains("quic://HOST:PORT"),
        "got {err:?}"
    );
    let routed = AddOpts {
        tls_server_name: Some("mini-route".to_owned()),
        ..opts()
    };
    let err = AddMode::classify("me@mini", None, &routed)
        .expect_err("a relay route belongs to the manual form");
    assert!(err.message.contains("--tls-server-name"), "got {err:?}");

    let named_pair = AddOpts {
        name: Some("x".to_owned()),
        ..opts()
    };
    assert!(
        AddMode::classify("mini", Some("ssh://mini"), &named_pair).is_err(),
        "--name and a NAME positional contradict"
    );
}

/// The three shared-middle failure classes map onto the error contract
/// with distinct, pasteable remedies.
#[test]
fn add_failures_carry_pasteable_remedies() {
    let err = add_failure_error(
        "mini",
        HostRole::Remote,
        &EnrollFailure::SshUnreachable("ssh: connect to host mini port 22: refused".to_owned()),
    );
    assert_eq!(err.code, codes::REGISTRY);
    assert!(
        err.message.contains("cannot reach mini over ssh"),
        "got {}",
        err.message
    );
    assert!(
        err.remedy.contains("`ssh mini`") && err.remedy.contains("`phux host add mini --ssh-only`"),
        "got {}",
        err.remedy
    );

    let err = add_failure_error(
        "mini",
        HostRole::Remote,
        &EnrollFailure::SshUnreachable(
            "ssh: connect to host mini port 22: Connection refused".to_owned(),
        ),
    );
    assert!(
        err.remedy.contains("Remote Login") && err.remedy.contains("tailscale set --ssh"),
        "a refused port 22 says how to turn ssh on: {}",
        err.remedy
    );

    let err = add_failure_error(
        "mini",
        HostRole::Remote,
        &EnrollFailure::MissingPhux("`phux` was not found on mini".to_owned()),
    );
    assert!(err.message.contains("no phux"), "got {}", err.message);
    assert!(
        err.remedy.contains("https://phux.sh/install") && err.remedy.contains("--remote-phux"),
        "the remedy names the install one-liner and the PATH escape: {}",
        err.remedy
    );

    assert!(
        err.remedy.contains("`phux host add mini --install`"),
        "the remedy offers to install it: {}",
        err.remedy
    );

    let err = add_failure_error(
        "mini",
        HostRole::Remote,
        &EnrollFailure::InstallFailed("the phux installer failed: unsupported platform".to_owned()),
    );
    assert!(
        err.message.contains("could not install phux on mini")
            && err.message.contains("unsupported platform"),
        "got {}",
        err.message
    );
    assert!(
        err.remedy
            .contains("ssh mini 'curl -fsSL https://phux.sh/install | sh'")
            && !err.remedy.contains("--install"),
        "a failed install is not answered with another --install: {}",
        err.remedy
    );

    let err = add_failure_error(
        "mini",
        HostRole::Remote,
        &EnrollFailure::Pair("remote `phux pair --json` reported no token".to_owned()),
    );
    assert!(
        err.message.contains("no token"),
        "the middle's message passes through: {}",
        err.message
    );
    assert!(
        err.remedy.contains("phux doctor")
            && err.remedy.contains("`phux host add mini --ssh-only`"),
        "got {}",
        err.remedy
    );
}

/// Under `--role satellite`, the pasteable remedy must carry the role,
/// or following it verbatim would register into the remote registry.
#[test]
fn add_failure_remedies_carry_the_satellite_role() {
    let err = add_failure_error(
        "mini",
        HostRole::Satellite,
        &EnrollFailure::Pair("remote `phux pair --json` reported no token".to_owned()),
    );
    assert!(
        err.remedy
            .contains("`phux host add mini --role satellite --ssh-only`"),
        "got {}",
        err.remedy
    );
}

/// Regression: a name escaping `<state>/remotes` (`../../evil`) must be
/// rejected before any token is written.
#[test]
fn enroll_rejects_a_traversal_name_before_writing_any_token() {
    let state_dir = tempfile::tempdir().expect("tempdir");

    let pairing = crate::commands::enroll::PairReport {
        ca_fingerprint: None,
        token: "deadbeef".to_owned(),
        cert_fingerprint: Some("ab".repeat(32)),
        overlay_addresses: vec!["100.64.0.2".to_owned()],
    };
    let route = Route {
        pairing: Some(&pairing),
        ..Route::bare("quic://mini:8788")
    };
    let err = finish_enroll_in(
        state_dir.path(),
        HostRole::Remote,
        "../../evil",
        &route,
        None,
        Some("me@mini"),
    )
    .expect_err("a traversal name must be refused");
    assert_eq!(err.code, codes::REGISTRY);

    // Nothing that looks like a bearer token landed anywhere under the
    // (fake) state dir — not at the intended path, and not at the
    // traversal target either.
    let found: Vec<_> = walkdir_tokens(state_dir.path()).collect();
    assert!(
        found.is_empty(),
        "rejection must not leave an orphaned token file: {found:?}"
    );
}

/// Regression: an unpinned `quic://` endpoint fails before `write_token`,
/// leaving no orphaned bearer token.
#[test]
fn enroll_rejects_unpinned_quic_before_writing_any_token() {
    let state_dir = tempfile::tempdir().expect("tempdir");

    let pairing = crate::commands::enroll::PairReport {
        ca_fingerprint: None,
        token: "deadbeef".to_owned(),
        cert_fingerprint: None,
        overlay_addresses: vec!["100.64.0.2".to_owned()],
    };
    let route = Route {
        pairing: Some(&pairing),
        ..Route::bare("quic://mini:8788")
    };
    let err = finish_enroll_in(
        state_dir.path(),
        HostRole::Remote,
        "mini",
        &route,
        None,
        Some("me@mini"),
    )
    .expect_err("an unpinned quic endpoint must be refused");
    assert!(err.message.contains("--cert-fingerprint"), "got {err:?}");

    let found: Vec<_> = walkdir_tokens(state_dir.path()).collect();
    assert!(
        found.is_empty(),
        "rejection must not leave an orphaned token file: {found:?}"
    );
}

/// An `ssh://` route with no direct candidate keeps no credential; one with a
/// candidate keeps token and pin; a satellite never keeps any beside `ssh://`.
#[test]
fn ssh_route_keeps_credentials_only_for_a_direct_candidate() {
    assert!(!keeps_credentials(HostRole::Remote, "ssh://me@mini", None));
    assert!(keeps_credentials(
        HostRole::Remote,
        "ssh://me@mini",
        Some("quic://100.64.0.2:8788")
    ));
    assert!(keeps_credentials(
        HostRole::Remote,
        "quic://mini:8788",
        None
    ));
    assert!(!keeps_credentials(
        HostRole::Satellite,
        "ssh://edge",
        Some("quic://100.64.0.2:8788")
    ));
    assert!(keeps_credentials(
        HostRole::Satellite,
        "quic://edge:8788",
        None
    ));
}

/// Every `*.token` file under `root`, walked without pulling in a full
/// directory-walking crate for two tests.
pub(super) fn walkdir_tokens(root: &Path) -> impl Iterator<Item = PathBuf> + use<> {
    fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "token") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    visit(root, &mut out);
    out.into_iter()
}
