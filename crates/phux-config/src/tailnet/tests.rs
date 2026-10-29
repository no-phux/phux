//! Unit tests for tailnet discovery and sign-in.

#![allow(clippy::expect_used, reason = "tests")]

use super::*;

fn parse(out: &str) -> Tailnet {
    parse_status(out, PathBuf::from("/bin/tailscale"))
}

const RUNNING: &str = r#"{
    "BackendState":"Running","AuthURL":"",
    "Self":{"HostName":"studio","DNSName":"studio.tail9a8b.ts.net.","OS":"macOS"},
    "CurrentTailnet":{"Name":"me@example.com","MagicDNSEnabled":true},
    "Peer":{
      "nodekey:a":{"HostName":"Mini","DNSName":"mini.tail9a8b.ts.net.","OS":"linux","Online":false,
                   "TailscaleIPs":["100.70.0.2","fd7a:115c:a1e0::2"],"sshHostKeys":["ssh-ed25519 AAAA"]},
      "nodekey:b":{"HostName":"Pat’s MacBook","DNSName":"pats-macbook.tail9a8b.ts.net.","OS":"macOS","Online":true,
                   "TailscaleIPs":["100.70.0.3"],"sshHostKeys":null},
      "nodekey:c":{"HostName":"localhost","DNSName":"phone.tail9a8b.ts.net.","OS":"iOS","Online":true,
                   "TailscaleIPs":["100.70.0.4"]},
      "nodekey:d":{"HostName":"se-sto","DNSName":"se-sto.mullvad.ts.net.","OS":"linux","Online":true,
                   "Location":{"Country":"Sweden"},"TailscaleIPs":["100.70.0.5"]}
    }}"#;

#[test]
fn a_running_client_lists_peers_connected_first() {
    let tailnet = parse(RUNNING);
    assert_eq!(tailnet.state, TailnetState::Running);
    assert_eq!(tailnet.name.as_deref(), Some("me@example.com"));
    assert_eq!(tailnet.self_name.as_deref(), Some("studio.tail9a8b.ts.net"));
    assert_eq!(tailnet.auth_url, None);
    let names: Vec<_> = tailnet
        .peers
        .iter()
        .map(|peer| peer.name.as_str())
        .collect();
    assert_eq!(names, ["pats-macbook", "phone", "mini"], "mullvad dropped");
    let mini = &tailnet.peers[2];
    assert_eq!(mini.dns_name.as_deref(), Some("mini.tail9a8b.ts.net"));
    assert_eq!(mini.host_name, "Mini");
    assert!(mini.tailscale_ssh && mini.supported() && !mini.online);
    assert_eq!(mini.ssh_host(), "mini.tail9a8b.ts.net");
    assert!(!tailnet.peers[1].supported(), "no phux for iOS");
    assert!(!tailnet.peers[0].tailscale_ssh);
}

#[test]
fn a_peer_is_named_by_its_short_name_dns_name_or_address() {
    let tailnet = parse(RUNNING);
    let mini = &tailnet.peers[2];
    for host in [
        "mini",
        "MINI",
        "mini.tail9a8b.ts.net",
        "mini.tail9a8b.ts.net.",
        "100.70.0.2",
    ] {
        assert!(mini.is_named_by(host), "{host}");
    }
    for host in ["", "mini2", "100.70.0.3", "studio"] {
        assert!(!mini.is_named_by(host), "{host}");
    }
}

#[test]
fn a_signed_out_client_reports_its_sign_in_link_and_no_peers() {
    let tailnet = parse(
        r#"{"BackendState":"NeedsLogin","AuthURL":"https://login.tailscale.com/a/abc",
            "Peer":{"nodekey:a":{"DNSName":"mini.ts.net.","OS":"linux"}}}"#,
    );
    assert_eq!(tailnet.state, TailnetState::NeedsLogin);
    assert_eq!(
        tailnet.auth_url.as_deref(),
        Some("https://login.tailscale.com/a/abc")
    );
    assert!(tailnet.peers.is_empty());
    assert_eq!(tailnet.name, None);
}

#[test]
fn backend_states_and_unreadable_output_classify() {
    for (doc, state) in [
        (r#"{"BackendState":"Stopped"}"#, TailnetState::Stopped),
        (r#"{"BackendState":"Starting"}"#, TailnetState::Starting),
        (
            r#"{"BackendState":"NeedsMachineAuth"}"#,
            TailnetState::NeedsMachineAuth,
        ),
        (r#"{"BackendState":"NoState"}"#, TailnetState::NotRunning),
        ("{}", TailnetState::NotRunning),
        (
            "failed to connect to local tailscaled",
            TailnetState::NotRunning,
        ),
    ] {
        assert_eq!(parse(doc).state, state, "{doc}");
    }
    // Only an https link is offered to open.
    let odd = parse(r#"{"BackendState":"NeedsLogin","AuthURL":"file:///etc/passwd"}"#);
    assert_eq!(odd.auth_url, None);
}

/// A stub `tailscale`: `status --json` prints `status.json`; `login`
/// runs `login.sh`.
#[cfg(unix)]
fn stub_cli(dir: &std::path::Path, status: &str, login: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(dir.join("status.json"), status).expect("status");
    std::fs::write(dir.join("login.sh"), login).expect("login");
    let cli = dir.join("tailscale");
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\ncd '{}'\ncase \"$1\" in status) cat status.json ;; login) exec sh login.sh ;; up) exit 1 ;; esac\n",
            dir.display()
        ),
    )
    .expect("stub");
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    cli
}

#[cfg(unix)]
#[test]
fn signing_in_returns_the_link_the_login_produced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cli = stub_cli(
        dir.path(),
        r#"{"BackendState":"NeedsLogin"}"#,
        r#"printf '%s' '{"BackendState":"NeedsLogin","AuthURL":"https://login.tailscale.com/a/x"}' > status.json; sleep 5"#,
    );
    let tailnet = sign_in_with(Some(&cli), || panic!("the app is not opened")).expect("link");
    assert_eq!(
        tailnet.auth_url.as_deref(),
        Some("https://login.tailscale.com/a/x")
    );
}

#[cfg(unix)]
#[test]
fn a_login_that_fails_at_once_is_an_error_with_its_words() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cli = stub_cli(
        dir.path(),
        r#"{"BackendState":"NeedsLogin"}"#,
        "echo 'access denied: run as root or set --operator' >&2; exit 1",
    );
    let start = std::time::Instant::now();
    let err = sign_in_with(Some(&cli), || false).expect_err("refused");
    assert!(err.message.contains("access denied"), "{err:?}");
    assert!(
        start.elapsed() < SIGN_IN_WAIT,
        "it does not wait out the deadline"
    );
}

#[cfg(unix)]
#[test]
fn signing_in_without_tailscale_or_a_reconnect_is_an_error() {
    let err = sign_in_with(None, || false).expect_err("not installed");
    assert!(err.remedy.contains("tailscale.com/download"), "{err:?}");
    let dir = tempfile::tempdir().expect("tempdir");
    let cli = stub_cli(dir.path(), r#"{"BackendState":"Stopped"}"#, "exit 0");
    let err = sign_in_with(Some(&cli), || false).expect_err("up refused");
    assert!(err.message.contains("tailscale up"), "{err:?}");
    let running = stub_cli(dir.path(), r#"{"BackendState":"Running"}"#, "exit 1");
    assert_eq!(
        sign_in_with(Some(&running), || false)
            .expect("running")
            .state,
        TailnetState::Running
    );
}

#[cfg(unix)]
#[test]
fn a_missing_cli_is_not_installed_and_a_dead_daemon_is_not_running() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let missing = discover_with(dir.path().join("no-such-tailscale"));
    assert_eq!(missing.state, TailnetState::NotInstalled);
    assert_eq!(missing.cli, None);

    let script = dir.path().join("tailscale");
    std::fs::write(
        &script,
        "#!/bin/sh\necho 'failed to connect to local Tailscale daemon' >&2\nexit 1\n",
    )
    .expect("write stub");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
    let dead = discover_with(script.clone());
    assert_eq!(dead.state, TailnetState::NotRunning);
    assert_eq!(dead.cli, Some(script));
}
