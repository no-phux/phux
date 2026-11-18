//! Where `--rec`, `--socket`, `--remote`, and the attach dial flags parse.

use super::*;

#[test]
fn every_attach_capable_root_path_is_classified_before_dispatch() {
    for (line, interactive) in [
        ("phux attach", true),
        ("phux new work", true),
        ("phux worktree new branch --attach", true),
        ("phux worktree open branch --attach", true),
        ("phux new -s work --json", false),
        ("phux worktree open branch", false),
        ("phux ls", false),
    ] {
        let cli = parse(line).expect("invocation parses");
        assert_eq!(crate::is_interactive_client(&cli), interactive, "{line}");
    }
}

/// `--rec` parses only on the root (naked `phux`) and on `attach`.
#[test]
fn rec_is_scoped_to_the_two_attaching_paths() {
    let cli = crate::parse_cli(["phux", "--rec", "demo.gif"]).expect("naked `phux --rec`");
    assert_eq!(
        cli.rec.rec.as_deref(),
        Some(std::path::Path::new("demo.gif"))
    );
    assert!(cli.command.is_none());

    let cli = crate::parse_cli(["phux", "attach", "work", "--rec", "demo.cast"])
        .expect("`phux attach NAME --rec PATH`");
    assert!(
        cli.rec.rec.is_none(),
        "the subcommand's own --rec is the one that carries the value"
    );
    let Some(Command::Attach(crate::commands::AttachArgs { session, rec, .. })) = cli.command
    else {
        panic!("expected Attach");
    };
    assert_eq!(session.as_deref(), Some("work"));
    assert_eq!(rec.rec.as_deref(), Some(std::path::Path::new("demo.cast")));

    // --size requires --composited, which conflicts with --from; `rec mark`
    // needs no --out; --rec is only on the two attaching paths, and
    // --rec-format needs a destination.
    assert_parses(&[
        (
            "phux rec --composited work --size 1920x1080 -o demo.cast",
            true,
        ),
        ("phux rec mark intro --json", true),
        ("phux rec --size 1920x1080 -o demo.cast", false),
        ("phux rec --composited --from demo.cast -o out.gif", false),
        ("phux ls --rec demo.gif", false),
        ("phux snapshot --rec demo.gif", false),
        ("phux --rec-format gif", false),
        ("phux attach --rec-format gif", false),
    ]);
}

/// A root `--rec` before a verb parses (so the global `--socket` can precede
/// a verb) and is refused post-parse.
#[test]
fn root_rec_before_a_verb_is_refused_post_parse() {
    for argv in [
        ["phux", "--rec", "demo.gif", "ls"].as_slice(),
        ["phux", "--rec", "demo.gif", "attach", "work"].as_slice(),
    ] {
        let cli = crate::parse_cli(argv)
            .expect("root --rec before a verb parses; the refusal is post-parse");
        let message = crate::refusals::root_rec_before_verb(&cli)
            .expect("a root --rec in front of a verb must be refused");
        assert!(
            message.contains("--rec") && message.contains("phux attach --rec"),
            "the refusal must teach the two correct spellings; got {message:?}"
        );
    }

    // The two legitimate homes stay untouched by the check.
    let cli = crate::parse_cli(["phux", "--rec", "demo.gif"]).expect("naked form");
    assert!(crate::refusals::root_rec_before_verb(&cli).is_none());
    let cli = crate::parse_cli(["phux", "attach", "--rec", "demo.gif"]).expect("attach");
    assert!(crate::refusals::root_rec_before_verb(&cli).is_none());
}

/// ADR-0180: on `--quic`, `--tls-server-name` is a relay route and
/// `--ca-fingerprint` the server behind it (canonicalized); the CA means
/// nothing anywhere else, and a WebSocket server name stays plain SNI.
#[test]
fn a_one_off_relay_hop_carries_the_route_and_the_servers_ca() {
    let ca = format!("sha256:{}", "AB".repeat(32));
    let hop =
        crate::commands::attach_cli::relay_hop(Some("relay:4433"), Some("mini-route"), Some(ca))
            .expect("hop")
            .expect("a relay route");
    assert_eq!(hop.route, "mini-route");
    assert_eq!(hop.ca, Some(format!("sha256:{}", "ab".repeat(32))));
    let unpinned =
        crate::commands::attach_cli::relay_hop(Some("relay:4433"), Some("mini-route"), None)
            .expect("planning refuses it later, by name")
            .expect("a relay route");
    assert_eq!(unpinned.ca, None);
    assert_eq!(
        crate::commands::attach_cli::relay_hop(Some("relay:4433"), None, None),
        Ok(None)
    );
    assert_eq!(
        crate::commands::attach_cli::relay_hop(None, Some("host"), None),
        Ok(None)
    );
    for (quic, route) in [(Some("relay:4433"), None), (None, Some("host"))] {
        let err = crate::commands::attach_cli::relay_hop(
            quic,
            route,
            Some(format!("sha256:{}", "ab".repeat(32))),
        )
        .expect_err("a CA without a relay route");
        assert!(err.contains("--tls-server-name"), "{err}");
    }
    assert!(
        crate::commands::attach_cli::relay_hop(
            Some("r:1"),
            Some("r"),
            Some("sha256:no".to_owned())
        )
        .is_err()
    );
}

/// usage-rs `requires("--quic", "--ws")` is all-of, so a single-transport
/// dial with `--token` (the documented `phux attach --quic HOST --token HEX`
/// form) used to fail at parse time. The any-of rule is post-parse.
#[test]
fn attach_dial_auth_flags_need_one_transport() {
    let refusal = |line: &str| {
        let Ok(Some(Command::Attach(crate::commands::AttachArgs {
            quic,
            ws,
            token,
            cert_fingerprint,
            tls_server_name,
            ..
        }))) = parse(line).map(|cli| cli.command)
        else {
            panic!("{line} must parse as Attach; any refusal is post-parse");
        };
        crate::commands::attach_cli::dial_auth_without_transport(
            quic.as_deref(),
            ws.as_deref(),
            token.as_deref(),
            cert_fingerprint.as_deref(),
            tls_server_name.as_deref(),
        )
    };
    for line in [
        "phux attach --quic 127.0.0.1:8788 --token ab",
        "phux attach --ws ws://127.0.0.1:8787 --token ab",
        "phux attach --quic 127.0.0.1:8788 --cert-fingerprint cd",
        "phux attach --ws wss://host:8787 --tls-server-name host",
    ] {
        assert_eq!(refusal(line), None, "{line} names a transport");
    }
    for flag in ["--token", "--cert-fingerprint", "--tls-server-name"] {
        let message = refusal(&format!("phux attach {flag} x"))
            .unwrap_or_else(|| panic!("{flag} must be refused without a transport"));
        assert!(
            message.contains("--quic") && message.contains("--ws"),
            "{flag} refusal must name both transports; got {message:?}"
        );
    }
}

/// The global `--socket` parses in both positions and lands on the same
/// root field either way; the two spellings are one invocation.
#[test]
fn socket_parses_before_and_after_the_verb() {
    let before = crate::parse_cli(["phux", "--socket", "/tmp/x.sock", "ls"])
        .expect("`phux --socket X ls` parses");
    let after = crate::parse_cli(["phux", "ls", "--socket", "/tmp/x.sock"])
        .expect("`phux ls --socket X` parses");
    for cli in [before, after] {
        assert!(matches!(cli.command, Some(Command::Ls(..))));
        assert_eq!(
            cli.socket.as_deref(),
            Some(std::path::Path::new("/tmp/x.sock"))
        );
    }
}

/// A `--socket` handed to a verb that never dials a server is refused
/// (via `socketless_verb`), not silently ignored.
#[test]
fn socketless_verbs_are_named_and_socket_consumers_are_not() {
    for (line, socketless) in [
        ("phux pair ls", true),
        ("phux pair revoke id", true),
        ("phux config path", true),
        ("phux plugin list", true),
        ("phux logs", true),
        ("phux completion zsh", true),
        ("phux ls", false),
        ("phux pair", false),
        ("phux config reload", false),
        ("phux tag ls work", false),
        ("phux service install", false),
        ("phux worktree list", false),
    ] {
        let cli = parse(&format!("{line} --socket /tmp/x.sock")).expect("--socket always parses");
        let command = cli.command.as_ref().expect("a verb was given");
        assert_eq!(
            crate::commands::socketless_verb(command).is_some(),
            socketless,
            "{line}: a socketless verb refuses --socket; a consumer keeps it"
        );
    }
}

/// `project link` and `unlink` take a session, `--json` and the verb's
/// own `--remote`, and dial a server, so `--socket` is theirs too.
#[test]
fn project_link_and_unlink_parse_their_target_and_remote() {
    use crate::commands::ProjectAction;
    let cli = parse("phux project link notes phux --json --remote me@mini").expect("link parses");
    let command = cli.command.as_ref().expect("a verb");
    let Command::Project(crate::commands::ProjectArgs {
        action:
            ProjectAction::Link {
                session,
                name,
                json,
                remote,
            },
    }) = command
    else {
        panic!("{command:?}");
    };
    assert_eq!(
        (session.as_str(), name.as_str(), *json),
        ("notes", "phux", true)
    );
    assert_eq!(remote.remote.as_deref(), Some("me@mini"));
    assert_eq!(crate::commands::verb_remote(command), Some("me@mini"));

    let cli = crate::parse_cli(["phux", "project", "unlink", "notes", "--socket", "/tmp/x"])
        .expect("unlink parses");
    let command = cli.command.as_ref().expect("a verb");
    assert!(matches!(
        command,
        Command::Project(crate::commands::ProjectArgs {
            action: ProjectAction::Unlink { session, json: false, .. }
        }) if session == "notes"
    ));
    assert!(crate::commands::socketless_verb(command).is_none());
    crate::parse_cli(["phux", "project", "link", "notes"]).expect_err("link needs a NAME");
}

/// ADR-0127: `--viewer` and `--take` parse on `attach`, refuse each
/// other at clap, and map onto the declared role every ATTACH carries.
#[test]
fn attach_viewer_and_take_parse_refuse_each_other_and_map_to_roles() {
    use phux_protocol::wire::frame::RolePolicy;
    // A viewer cannot take over: the flags conflict at parse time.
    assert_parses(&[
        ("phux attach --viewer", true),
        ("phux attach --take work", true),
        ("phux attach --viewer --take", false),
    ]);
    assert_eq!(
        crate::commands::attach_cli::declared_attach_role(true, false),
        RolePolicy::VIEWER
    );
    assert_eq!(
        crate::commands::attach_cli::declared_attach_role(false, true),
        RolePolicy::TAKEOVER
    );
    assert_eq!(
        crate::commands::attach_cli::declared_attach_role(false, false),
        RolePolicy::PRIMARY,
        "no flag is the default attach, which writes no role byte"
    );
}
