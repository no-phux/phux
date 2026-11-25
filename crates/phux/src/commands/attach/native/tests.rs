//! Native CLI credential diagnostics and shared-planner parity fixtures.

use phux_dial::CertTrust;

use super::*;

mod quic;
mod ws;

type NativeCase<'a> = (
    &'a str,
    &'a str,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    bool,
);

    /// The typed refusal renders the exact lines attach printed before the
    /// planners stopped printing: the flag-naming remedy for attach, and the
    /// overlay hint only after a DNS name failed to resolve.
    #[test]
    fn dial_refusals_keep_attach_wording() {
        let unpinned = DialRefusal::UnpinnedQuic {
            target: "mini:8788".to_owned(),
        }
        .attach_lines();
        assert_eq!(
            unpinned,
            vec![
                "refusing to dial non-loopback QUIC server mini:8788 without --cert-fingerprint.",
                "Run `phux pair` on the server host to print its certificate fingerprint,",
                "then pass it: phux attach --quic mini:8788 --cert-fingerprint <FP>",
            ]
        );

        let unresolved = |dns_name| {
            DialRefusal::Unresolved {
                target: "mini:8788".to_owned(),
                detail: "name resolution failed: nope".to_owned(),
                dns_name,
            }
            .attach_lines()
        };
        assert_eq!(
            unresolved(true),
            vec![
                "QUIC attach to mini:8788 failed: name resolution failed: nope".to_owned(),
                OVERLAY_REACHABILITY_HINT.to_owned(),
            ]
        );
        assert_eq!(
            unresolved(false).len(),
            1,
            "an IP literal never earns the hint"
        );

        assert_eq!(
            DialRefusal::NoToken {
                url: "wss://h:1".to_owned()
            }
            .attach_lines()[0],
            "refusing remote WebSocket attach to wss://h:1 without --token."
        );
    }
    /// `--quic` targets split on the last `:`, so IPv4 literals, bracketed
    /// IPv6 literals, and DNS names all parse; a missing or malformed port
    /// is rejected up front with a usage error.
    #[test]
    fn split_host_port_accepts_documented_target_shapes() {
        assert_eq!(
            split_host_port("127.0.0.1:8788"),
            Ok(("127.0.0.1", 8788_u16))
        );
        assert_eq!(split_host_port("[::1]:1"), Ok(("[::1]", 1_u16)));
        assert_eq!(
            split_host_port("myhost.tailnet-name.ts.net:8788"),
            Ok(("myhost.tailnet-name.ts.net", 8788_u16))
        );

        for (target, reason) in [
            ("myhost.tailnet-name.ts.net", "missing a port"),
            ("myhost:notaport", "invalid port"),
            (":8788", "missing a host"),
        ] {
            let err = split_host_port(target).expect_err(target);
            assert!(err.contains(reason), "{target}: {err}");
        }
    }

    /// A relay route with no CA pinned is refused by the name the caller
    /// gave (the registry entry), not the dialed address, with the CLI's
    /// own remedy appended to the runtime's transport-neutral reason.
    #[test]
    fn an_unpinned_relay_route_is_refused_by_name_with_the_cli_remedy() {
        let rt = crate::commands::build_cli_runtime().expect("runtime");
        let credentials = RemoteCredentials {
            name: Some("studio".into()),
            token: None,
            cert_fingerprint: Some("ab".repeat(32)),
            authority: phux_client_runtime::target::AuthorityPin {
                ca: None,
                route: Some("studio-route".into()),
            },
            identity: Some(phux_dial::TlsClientIdentity::None),
        };
        let Err(DialRefusal::Malformed(refused)) =
            plan_quic_dial(&rt, "192.0.2.1:4433", credentials, None)
        else {
            panic!("a relay route without the server's CA must be refused");
        };
        assert!(refused.starts_with("studio is reached"), "{refused}");
        assert!(!refused.contains("192.0.2.1"), "{refused}");
        assert!(refused.contains("--relay-route studio-route"), "{refused}");
        assert!(refused.contains("--ca-fingerprint"), "{refused}");
    }

    fn parity_credentials(
        leaf: Option<&str>,
        ca: Option<&str>,
        route: Option<&str>,
        token: Option<&str>,
    ) -> RemoteCredentials {
        RemoteCredentials {
            name: Some("fixture".into()),
            token: token.map(str::to_owned),
            cert_fingerprint: leaf.map(str::to_owned),
            authority: phux_client_runtime::target::AuthorityPin {
                ca: ca.map(str::to_owned),
                route: route.map(str::to_owned),
            },
            identity: Some(phux_dial::TlsClientIdentity::RequirePaired {
                certificate: "/not-read-during-planning/client.pem".into(),
                private_key: "/not-read-during-planning/client.key".into(),
            }),
        }
    }
