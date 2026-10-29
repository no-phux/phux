use super::*;
use crate::commands::doctor::Status;

/// A bound-but-silent listener fails with a firewall remedy; benign states
/// warn; a server-reported disable points at restart, not pairing.
#[test]
fn a_bound_but_silent_listener_fails_with_an_actionable_remedy() {
    let url = "wss://100.64.0.2:8787";

    let check = remote_reachable_check(url, Reachability::Silent, None);
    assert_eq!(
        check.status,
        Status::Fail,
        "a listener that answers nothing is a failure, not a warning: \
         remote access is entirely broken"
    );
    assert!(
        check.detail.contains("UDS"),
        "the detail must contrast UDS-healthy with the silent remote: {}",
        check.detail
    );
    assert!(
        check.detail.contains("firewall") || check.detail.contains("stealth"),
        "the detail must name the failure mode, not a dead server: {}",
        check.detail
    );
    let hint = check.hint.expect("a failure must carry a remedy");
    assert!(
        hint.contains("firewall") || hint.contains("packet filter"),
        "the remedy must name the blocker: {hint}"
    );
    #[cfg(target_os = "macos")]
    {
        assert!(
            hint.contains("adhoc") && hint.contains("Cellar"),
            "macOS remedy must name the adhoc signature and the versioned Cellar path: {hint}"
        );
    }

    assert_eq!(
        remote_reachable_check(url, Reachability::Answered, None).status,
        Status::Pass
    );

    for benign in [Reachability::NoListener, Reachability::Unreachable] {
        assert_eq!(
            remote_reachable_check(url, benign, None).status,
            Status::Warn,
            "{benign:?} is a normal local-only state, not a broken install"
        );
    }

    let check = remote_reachable_check(
        url,
        Reachability::NoListener,
        Some(phux_protocol::wire::ListenerDisabledReason::TokenStoreLoadFailed),
    );
    let hint = check.hint.expect("hint");
    assert!(
        hint.contains("restart") || hint.contains("upgrade"),
        "server-disabled wss must name restart, not pair: {hint}"
    );
    assert!(
        !hint.contains("phux pair"),
        "must not blame pairing when the server already explained the disable: {hint}"
    );
}

/// Bound-address classification, without dialing.
#[test]
fn off_loopback_target_classifies_bound_addresses_without_dialing() {
    let overlay = [IpAddr::V4(std::net::Ipv4Addr::new(100, 64, 0, 2))];

    assert_eq!(
        off_loopback_target(Some("192.0.2.10:9443"), &[]),
        OffLoopbackTarget::Dial("192.0.2.10:9443".parse().unwrap()),
        "a concrete non-loopback bind is probeable without overlay detect"
    );
    assert_eq!(
        off_loopback_target(Some("127.0.0.1:8787"), &overlay),
        OffLoopbackTarget::LoopbackOnly
    );
    assert_eq!(
        off_loopback_target(Some("[::1]:8787"), &overlay),
        OffLoopbackTarget::LoopbackOnly
    );
    assert_eq!(
        off_loopback_target(Some("0.0.0.0:9443"), &overlay),
        OffLoopbackTarget::Dial(SocketAddr::from(([100, 64, 0, 2], 9443))),
        "unspecified bind keeps the bound port, not the default 8787"
    );
    assert!(matches!(
        off_loopback_target(Some("0.0.0.0:8787"), &[]),
        OffLoopbackTarget::NoOffLoopback { .. }
    ));
    assert!(matches!(
        off_loopback_target(None, &overlay),
        OffLoopbackTarget::NoOffLoopback { .. }
    ));
}

#[test]
fn wildcard_probe_selects_only_the_listener_address_family() {
    let v4: IpAddr = "10.77.0.2".parse().unwrap();
    let v6: IpAddr = "fd77::2".parse().unwrap();
    assert_eq!(
        off_loopback_target(Some("[::]:9443"), &[v4, v6]),
        OffLoopbackTarget::Dial(SocketAddr::new(v6, 9443)),
    );
    assert_eq!(
        off_loopback_target(Some("0.0.0.0:9443"), &[v6, v4]),
        OffLoopbackTarget::Dial(SocketAddr::new(v4, 9443)),
    );
    for (bound, other_family) in [("[::]:9443", v4), ("0.0.0.0:9443", v6)] {
        let target = off_loopback_target(Some(bound), &[other_family]);
        assert!(matches!(target, OffLoopbackTarget::NoOffLoopback { .. }));
        let verdict = remote_reachable_verdict(target, |_| {
            unreachable!("family mismatch must warn, not dial another listener")
        });
        assert_eq!(verdict.status, Status::Warn);
    }
}

#[test]
fn loopback_bind_passes_and_unspecified_without_overlay_warns_instead_of_passing() {
    let loopback = remote_reachable_verdict(OffLoopbackTarget::LoopbackOnly, |_| {
        unreachable!("loopback-only must not dial")
    });
    assert_eq!(loopback.status, Status::Pass);

    let unspecified = remote_reachable_verdict(
        OffLoopbackTarget::NoOffLoopback {
            bound: "0.0.0.0:8787".into(),
        },
        |_| unreachable!("no off-loopback target must not dial"),
    );
    assert_eq!(
        unspecified.status,
        Status::Warn,
        "passing hid the firewall stealth-drop when overlay detect was empty"
    );
    assert!(
        unspecified.detail.contains("0.0.0.0:8787"),
        "{}",
        unspecified.detail
    );

    let silent = remote_reachable_verdict(
        OffLoopbackTarget::Dial("192.0.2.10:8787".parse().unwrap()),
        |_| Reachability::Silent,
    );
    assert_eq!(silent.status, Status::Fail);
    assert!(silent.detail.contains("wss://192.0.2.10:8787"));
    assert!(silent.detail.contains("UDS"));
}

/// Server-reported disabled listeners fail with a restart hint when the
/// store now loads.
#[test]
fn server_reported_disabled_listeners_fail_with_a_restart_hint_when_the_store_is_fine() {
    use phux_protocol::wire::{
        ListenerDisabledReason, RemoteListenerSlot, RemoteListenerTransport, RemoteListenersReport,
    };

    let report = RemoteListenersReport::new().with_listeners(vec![
        RemoteListenerSlot::disabled(
            RemoteListenerTransport::Wss,
            Some("100.64.0.2:8787".into()),
            ListenerDisabledReason::TokenStoreLoadFailed,
        ),
        RemoteListenerSlot::disabled(
            RemoteListenerTransport::Quic,
            Some("100.64.0.2:8788".into()),
            ListenerDisabledReason::TokenStoreLoadFailed,
        ),
    ]);

    let check = remote_listeners_check(Some(&report), true);
    assert_eq!(check.status, Status::Fail);
    assert!(
        check.detail.contains("wss") && check.detail.contains("quic"),
        "detail must name every disabled transport: {}",
        check.detail
    );
    let hint = check.hint.expect("hint");
    assert!(
        hint.contains("loads now") && (hint.contains("restart") || hint.contains("upgrade")),
        "a healthy store against a disabled server must say restart: {hint}"
    );

    let bound = RemoteListenersReport::new().with_listeners(vec![RemoteListenerSlot::bound(
        RemoteListenerTransport::Wss,
        "127.0.0.1:8787",
    )]);
    assert_eq!(
        remote_listeners_check(Some(&bound), true).status,
        Status::Pass
    );
    assert_eq!(remote_listeners_check(None, true).status, Status::Pass);
}

/// A listener a resumed server left unbound for its policy names the
/// mode as the remedy, not a bind error (ADR-0179).
#[test]
fn a_policy_refused_listener_names_the_mode() {
    use phux_protocol::wire::{
        ListenerDisabledReason, RemoteListenerSlot, RemoteListenerTransport, RemoteListenersReport,
    };

    let report = RemoteListenersReport::new().with_listeners(vec![RemoteListenerSlot::disabled(
        RemoteListenerTransport::Quic,
        Some("0.0.0.0:8788".into()),
        ListenerDisabledReason::PolicyModeRefused,
    )]);
    let check = remote_listeners_check(Some(&report), true);
    assert_eq!(check.status, Status::Fail);
    assert!(
        check.detail.contains("policy_mode_refused"),
        "{}",
        check.detail
    );
    let hint = check.hint.expect("hint");
    assert!(
        hint.contains("mode = \"paired\"") && hint.contains("phux upgrade"),
        "{hint}"
    );
}

/// A dead socket may still be called stale. A live incompatible server fails
/// with the stop-and-start recovery and is not told to remove the socket.
#[test]
fn an_incompatible_remote_probe_is_not_told_to_remove_the_socket() {
    use phux_client::attach::AttachError;

    let dead = AttachError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
    let dead_check = remote_probe_failure(std::path::Path::new("/tmp/dead.sock"), &dead, None);
    assert_eq!(dead_check.status, Status::Warn);
    let dead_hint = dead_check.hint.expect("hint");
    assert!(dead_hint.contains("stale"), "{dead_hint}");
    assert!(
        dead_hint.contains("remove it and start a fresh server"),
        "{dead_hint}"
    );

    let err = AttachError::IncompatibleProtocol(
        "incompatible protocol: client offered 1.4.0, server requires 0.9.x; \
         update the phux server so protocol major.minor match"
            .to_owned(),
    );
    let check = remote_probe_failure(std::path::Path::new("/tmp/old.sock"), &err, Some(4242));
    assert_eq!(check.status, Status::Fail);
    let text = format!("{}\n{}", check.detail, check.hint.expect("hint"));
    assert!(text.contains("kill 4242"), "{text}");
    assert!(
        text.contains("`ps -p 4242 -o pid=,ppid=,user=,args=`"),
        "{text}"
    );
    assert!(
        text.contains("phux server --ensure --socket '/tmp/old.sock'`"),
        "{text}"
    );
    for forbidden in [
        "stale",
        "phux upgrade",
        "survive",
        "remove",
        "pkill",
        "killall",
    ] {
        assert!(!text.contains(forbidden), "{forbidden} in {text}");
    }

    let other = AttachError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    let other_check = remote_probe_failure(std::path::Path::new("/tmp/denied.sock"), &other, None);
    assert_eq!(other_check.status, Status::Warn);
    let other_hint = other_check.hint.expect("hint");
    assert!(!other_hint.contains("stale"), "{other_hint}");
    assert!(!other_hint.contains("remove"), "{other_hint}");
}
