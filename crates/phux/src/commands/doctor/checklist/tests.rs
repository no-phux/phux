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
