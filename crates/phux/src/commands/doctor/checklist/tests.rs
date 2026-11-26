use phux_protocol::wire::{RemoteListenerSlot, RemoteListenerTransport};

use super::*;

fn integration(agent: &'static str, installed: Option<bool>, install: &'static str) -> Integration {
    Integration {
        agent,
        installed,
        install,
    }
}

fn server(listeners: Vec<RemoteListenerSlot>, devices: Vec<&str>) -> ServerInputs {
    ServerInputs {
        listeners: Some(RemoteListenersReport::new().with_listeners(listeners)),
        push_devices: Some(devices.into_iter().map(str::to_owned).collect()),
        file_download: true,
    }
}

fn inputs(server: Option<ServerInputs>, supervision: Supervision) -> Inputs {
    Inputs {
        integrations: Ok(vec![
            integration("Claude Code", Some(true), "claude plugin install phux@phux"),
            integration(
                "OpenCode",
                Some(false),
                "opencode plugin add @phux/opencode",
            ),
            integration("Codex", None, ""),
        ]),
        shim: Some(Check::pass(
            "agent-shim",
            "no claude-in-phux shim installed",
        )),
        skill: Some(Check::pass(
            "skill",
            "phux skill installed for claude, codex",
        )),
        reachable: None,
        server,
        supervision,
    }
}

/// The `checklist` contract clients parse (`docs/troubleshooting.md`):
/// schema version, item ids in order, and every row's keys. Changing this
/// test means changing the schema.
#[test]
fn the_checklist_document_is_the_versioned_contract() {
    let quic = RemoteListenerSlot::disabled(
        RemoteListenerTransport::Quic,
        Some("100.64.0.1:8788".to_owned()),
        ListenerDisabledReason::BindFailed,
    );
    let doc = to_json(&items(&inputs(
        Some(server(vec![quic], vec!["phone-1"])),
        Supervision::Unsupervised,
    )));
    assert_eq!(
        doc,
        serde_json::json!({
            "schema_version": 1,
            "items": [
                {
                    "id": "agent_status",
                    "title": "Agent status",
                    "status": "off",
                    "reason": "no agent integration for OpenCode",
                    "fix_command": "opencode plugin add @phux/opencode",
                    "fix_action": null,
                },
                {
                    "id": "notifications",
                    "title": "Asks and notifications",
                    "status": "ok",
                    "reason": "1 phone gets notifications from this host",
                    "fix_command": null,
                    "fix_action": null,
                    "push_devices": ["phone-1"],
                },
                {
                    "id": "remote_reach",
                    "title": "Remote reach",
                    "status": "off",
                    "reason": "quic listener not bound: the address could not be bound; the server log says why",
                    "fix_command": "phux upgrade",
                    "fix_action": null,
                },
                {
                    "id": "file_transfer",
                    "title": "File transfer",
                    "status": "ok",
                    "reason": "files can be sent to this host and fetched from it",
                    "fix_command": null,
                    "fix_action": null,
                },
                {
                    "id": "restore",
                    "title": "Restore after restart",
                    "status": "off",
                    "reason": "server not supervised, sessions end at logout",
                    "fix_command": "phux service install --adopt",
                    "fix_action": null,
                },
                {
                    "id": "skill",
                    "title": "Agent skill",
                    "status": "ok",
                    "reason": "phux skill installed for claude, codex",
                    "fix_command": null,
                    "fix_action": null,
                },
            ],
        })
    );
}

/// With no server, every server-side item says so and how to start one;
/// nothing claims to work.
#[test]
fn no_server_makes_server_items_unknown_with_a_start_command() {
    let items = items(&inputs(None, Supervision::Unsupervised));
    for id in ["notifications", "remote_reach", "file_transfer"] {
        let item = items.iter().find(|item| item.id == id).unwrap();
        assert_eq!(item.verdict, Verdict::Unknown, "{id}");
        assert_eq!(item.fix_command.as_deref(), Some(START_SERVER), "{id}");
    }
    let restore = items.iter().find(|item| item.id == "restore").unwrap();
    assert_eq!(restore.fix_command.as_deref(), Some("phux service install"));
}

/// No phone registered is the one item a client fixes in one tap.
#[test]
fn no_push_device_offers_the_enable_notifications_action() {
    let item = notifications(Some(&server(Vec::new(), Vec::new())));
    assert_eq!(item.verdict, Verdict::Off);
    assert_eq!(item.fix_action, Some("enable_notifications"));
}

/// Bound listeners and a supervised, lingering server read as working.
#[test]
fn bound_listeners_and_supervision_read_as_working() {
    let wss = RemoteListenerSlot::bound(RemoteListenerTransport::Wss, "100.64.0.1:8787");
    let reach = remote_reach(Some(&server(vec![wss], Vec::new())), None);
    assert_eq!(reach.verdict, Verdict::Ok);
    assert_eq!(reach.reason, "listening on wss 100.64.0.1:8787");
    let systemd = restore(
        Supervision::Running {
            manager: "systemd",
            linger: false,
        },
        true,
    );
    assert_eq!(systemd.verdict, Verdict::Off);
    assert_eq!(
        systemd.fix_command.as_deref(),
        Some("loginctl enable-linger")
    );
}

/// A missing listener table or an empty one both mean nothing remote can
/// reach this host, and pairing is the fix.
#[test]
fn no_listener_points_at_pairing() {
    let item = remote_reach(Some(&server(Vec::new(), Vec::new())), None);
    assert_eq!(item.verdict, Verdict::Off);
    assert_eq!(item.fix_command.as_deref(), Some("phux pair"));
}

/// A not-installed skill is a fix; a stale one too.
#[test]
fn a_missing_skill_points_at_skill_setup() {
    let check = Check {
        hint: Some("run `phux skill setup` to teach your agents about phux".to_owned()),
        ..Check::pass("skill", "no phux skill installed for any agent")
    };
    let item = skill(Some(&check));
    assert_eq!(item.verdict, Verdict::Off);
    assert_eq!(item.fix_command.as_deref(), Some("phux skill setup"));
}

/// A bind the server is retrying reads as waiting, in plain words, with no
/// command: it heals by itself once the address exists.
#[test]
fn a_retried_bind_is_waiting_not_off() {
    let quic = RemoteListenerSlot::waiting(
        RemoteListenerTransport::Quic,
        "100.64.0.1:8788",
        "Cannot assign requested address (os error 99)",
    );
    let item = remote_reach(Some(&server(vec![quic], Vec::new())), None);
    assert_eq!(item.verdict, Verdict::Waiting);
    assert_eq!(
        item.reason,
        "quic listener waiting to bind: address not available yet"
    );
    assert_eq!(item.fix_command, None);
}

/// An overlay listener waiting for its address beside a bound configured
/// one reads as waiting, not as listening (#2430).
#[test]
fn a_listener_waiting_for_its_address_is_waiting_not_ok() {
    let quic = RemoteListenerSlot::bound(RemoteListenerTransport::Quic, "100.64.0.1:8788");
    let wss = RemoteListenerSlot::waiting_for_address(RemoteListenerTransport::Wss);
    let item = remote_reach(Some(&server(vec![quic, wss], Vec::new())), None);
    assert_eq!(item.verdict, Verdict::Waiting);
    assert_eq!(
        item.reason,
        "wss listener waiting for a private network address; it starts once one appears"
    );
    assert_eq!(item.fix_command, None);
}

/// A capability that is off is a neutral mark, never a red failure: the
/// checklist does not change doctor's exit code. Fixes sit under the reason,
/// one command each, and a verdict newer than this binary reads as unknown.
#[test]
fn the_rendered_checklist_marks_off_as_neutral_with_fixes_under_it() {
    let doc = serde_json::json!({"items": [
        {"title": "Agent status", "status": "off", "reason": "no integration",
         "fix_command": "a install\nb install"},
        {"title": "File transfer", "status": "ok", "reason": "works"},
        {"title": "Remote reach", "status": "waiting", "reason": "binding"},
        {"title": "Restore", "status": "someday", "reason": "new verdict"},
    ]});
    let text = render_doc(&doc, "What works on mini", false, &|c| format!("run {c}"));
    // The mark, a space, the 13-wide title column and its two-space gutter.
    let under = " ".repeat(1 + 1 + 13 + 2);
    assert_eq!(
        text,
        format!(
            "\nWhat works on mini\n\
             – Agent status   no integration\n\
             {under}fix: run a install\n\
             {under}fix: run b install\n\
             ✓ File transfer  works\n\
             … Remote reach   binding\n\
             ? Restore        new verdict\n"
        )
    );
    let colored = render_doc(&doc, "h", true, &str::to_owned);
    assert!(
        colored.contains("\n– Agent status"),
        "off is never coloured: {colored}"
    );
    assert!(
        colored.contains("\x1b[32m✓\x1b[0m File transfer"),
        "{colored}"
    );
}
