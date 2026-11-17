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
        code: None,
        hardware_key: false,
        session: None,
        disabled: false,
        json: JsonOpt { json: false },
    }
}

/// `--hardware-key` is refused where no client certificate is enrolled, and
/// unasked it checks nothing.
#[test]
fn hardware_key_applies_only_where_a_certificate_is_enrolled() {
    use super::add::{hardware_key_misplaced, hardware_key_ready};
    let manual = AddMode::Manual {
        name: "mini".to_owned(),
        endpoint: "quic://mini:8788".to_owned(),
    };
    let ssh = AddMode::Ssh {
        target: "me@mini".to_owned(),
    };
    let code = AddMode::Code {
        name: "mini".to_owned(),
    };
    assert!(hardware_key_misplaced(HostRole::Remote, Some(&manual)).is_some());
    assert!(hardware_key_misplaced(HostRole::Satellite, Some(&ssh)).is_some());
    assert!(hardware_key_misplaced(HostRole::Remote, Some(&ssh)).is_none());
    assert!(hardware_key_misplaced(HostRole::Remote, Some(&code)).is_none());
    assert!(hardware_key_ready(false).is_ok());
}

/// A machine with no Secure Enclave refuses the flag in plain words, before
/// anything is contacted.
#[cfg(not(target_os = "macos"))]
#[test]
fn hardware_key_is_refused_without_a_secure_enclave() {
    let err = super::add::hardware_key_ready(true).expect_err("no enclave");
    assert_eq!(err.code, codes::WORKLOAD);
    assert!(err.message.contains("Secure Enclave"), "{}", err.message);
    assert!(err.remedy.contains("nothing was changed"), "{}", err.remedy);
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

/// The shared-middle failure classes map onto the error contract with
/// distinct, pasteable remedies: a refused port 22 says how to turn ssh on,
/// an unknown key how to check it, a login sshd dropped names `MaxStartups`,
/// and a failed install is not answered with another `--install`. Under
/// `--role satellite` the remedy carries the role, or following it verbatim
/// would register into the remote registry.
#[test]
fn add_failures_carry_pasteable_remedies() {
    let ssh = |text: &str| EnrollFailure::SshUnreachable(text.to_owned());
    let no_token = || EnrollFailure::Pair("remote `phux pair --json` reported no token".to_owned());
    for (role, failure, message, remedy, not_remedy) in [
        (
            HostRole::Remote,
            ssh("ssh: connect to host mini port 22: refused"),
            &["cannot reach mini over ssh"][..],
            &["`ssh mini`", "`phux host add mini --ssh-only`"][..],
            None,
        ),
        (
            HostRole::Remote,
            ssh("ssh: connect to host mini port 22: Connection refused"),
            &[],
            &["Remote Login", "tailscale set --ssh"],
            None,
        ),
        (
            HostRole::Remote,
            ssh("kex_exchange_identification: Connection closed by remote host"),
            &["cannot reach mini over ssh"],
            &["MaxStartups"],
            None,
        ),
        (
            HostRole::Remote,
            ssh("Host key verification failed."),
            &["host key"],
            &["`ssh mini`"],
            None,
        ),
        (
            HostRole::Remote,
            EnrollFailure::MissingPhux("`phux` was not found on mini".to_owned()),
            &["no phux"],
            &[
                "https://phux.sh/install",
                "--remote-phux",
                "`phux host add mini --install`",
            ],
            None,
        ),
        (
            HostRole::Remote,
            EnrollFailure::InstallFailed(
                "the phux installer failed: unsupported platform".to_owned(),
            ),
            &["could not install phux on mini", "unsupported platform"],
            &["ssh mini 'curl -fsSL https://phux.sh/install | sh'"],
            Some("--install"),
        ),
        (
            HostRole::Remote,
            no_token(),
            &["no token"],
            &["phux doctor", "`phux host add mini --ssh-only`"],
            None,
        ),
        (
            HostRole::Satellite,
            no_token(),
            &[],
            &["`phux host add mini --role satellite --ssh-only`"],
            None,
        ),
    ] {
        let err = add_failure_error("mini", role, &failure);
        assert_eq!(err.code, codes::REGISTRY);
        for needle in message {
            assert!(err.message.contains(needle), "{needle}: {}", err.message);
        }
        for needle in remedy {
            assert!(err.remedy.contains(needle), "{needle}: {}", err.remedy);
        }
        if let Some(absent) = not_remedy {
            assert!(!err.remedy.contains(absent), "{absent}: {}", err.remedy);
        }
    }
}

/// Regression: a name escaping `<state>/remotes` (`../../evil`), or an
/// unpinned `quic://` endpoint, is refused before any bearer token is
/// written, at the intended path or the traversal target.
#[test]
fn enroll_refusals_write_no_token() {
    for (name, cert_fingerprint, needle) in [
        ("../../evil", Some("ab".repeat(32)), "must not contain"),
        ("mini", None, "--cert-fingerprint"),
    ] {
        let state_dir = tempfile::tempdir().expect("tempdir");
        let pairing = crate::commands::enroll::PairReport {
            ca_fingerprint: None,
            token: "deadbeef".to_owned(),
            cert_fingerprint,
            overlay_addresses: vec!["100.64.0.2".to_owned()],
        };
        let route = Route {
            pairing: Some(&pairing),
            ..Route::bare("quic://mini:8788")
        };
        let err = finish_enroll_in(
            state_dir.path(),
            HostRole::Remote,
            name,
            &route,
            None,
            Some("me@mini"),
        )
        .expect_err("refused");
        assert_eq!(err.code, codes::REGISTRY);
        assert!(err.message.contains(needle), "{name}: {err:?}");
        let found: Vec<_> = walkdir_tokens(state_dir.path()).collect();
        assert!(found.is_empty(), "orphaned token file: {found:?}");
    }
}

/// An `ssh://` route with no direct candidate keeps no credential; one with a
/// candidate keeps token and pin; a satellite never keeps any beside `ssh://`.
#[test]
fn ssh_route_keeps_credentials_only_for_a_direct_candidate() {
    let quic = Some("quic://100.64.0.2:8788");
    for (role, endpoint, candidate, keeps) in [
        (HostRole::Remote, "ssh://me@mini", None, false),
        (HostRole::Remote, "ssh://me@mini", quic, true),
        (HostRole::Remote, "quic://mini:8788", None, true),
        (HostRole::Satellite, "ssh://edge", quic, false),
        (HostRole::Satellite, "quic://edge:8788", None, true),
    ] {
        assert_eq!(
            keeps_credentials(role, endpoint, candidate),
            keeps,
            "{endpoint}"
        );
    }
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

/// `NAME --code LINK` is its own form: the link names the endpoint and every
/// credential, so a flag that names one too, or the ssh form's, is refused.
#[test]
fn add_mode_code_form_takes_only_a_name() {
    let code = || AddOpts {
        code: Some("-".to_owned()),
        ..opts()
    };
    assert_eq!(
        AddMode::classify("mini", None, &code()).expect("code"),
        AddMode::Code {
            name: "mini".to_owned()
        }
    );
    let refused = [
        ("an ENDPOINT", Some("quic://mini:8788"), code()),
        (
            "--install",
            None,
            AddOpts {
                install: true,
                ..code()
            },
        ),
        (
            "--name",
            None,
            AddOpts {
                name: Some("x".to_owned()),
                ..code()
            },
        ),
        (
            "--role satellite",
            None,
            AddOpts {
                role: HostRole::Satellite,
                ..code()
            },
        ),
        (
            "a manual credential flag",
            None,
            AddOpts {
                ca_fingerprint: Some("sha256:00".to_owned()),
                ..code()
            },
        ),
    ];
    for (conflict, endpoint, opts) in refused {
        let err = AddMode::classify("mini", endpoint, &opts).expect_err(conflict);
        assert!(
            err.message.contains(conflict),
            "{conflict}: {}",
            err.message
        );
        assert!(err.remedy.contains("--code LINK"), "{}", err.remedy);
    }
}
