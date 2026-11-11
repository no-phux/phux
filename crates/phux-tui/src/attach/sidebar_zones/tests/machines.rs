//! ADR-0140 machine segments: other machines in the session roster.

use super::*;

/// ADR-0140: attached to `mini`, the roster reads as three machine
/// segments: mini's live sessions first (labelled by its registry name,
/// not re-listed from the provider), then this laptop and a host that is
/// down, each routing a click through `switch-host`.
#[test]
fn other_machines_append_as_switchable_segments() {
    use phux_core::host_list::HostKind;
    let f = fixture();
    let origin = crate::attach::hosts::AttachOrigin::Remote("mini".to_owned());
    let hosts = vec![
        machine("local", "laptop", HostKind::Local, true, &["work"]),
        machine("mini", "mini", HostKind::Remote, true, &["here", "peer"]),
        machine("xps", "xps", HostKind::Remote, false, &[]),
    ];
    let peers = PeerInputs {
        origin: Some(&origin),
        remote_hosts: &hosts,
        ..f.inputs()
    };
    let roster = session_roster(&peers, &[]);
    let rows: Vec<_> = roster
        .iter()
        .map(|r| {
            (
                r.host.as_str(),
                r.name.as_str(),
                r.switch_host.as_deref(),
                r.selectable,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("mini", "here", None, true),
            ("mini", "peer", None, true),
            ("laptop", "work", Some("local"), true),
            ("xps", "down", Some("xps"), false),
        ]
    );
}

/// Without a recorded origin (tests, embedders) nothing is appended and
/// the attached server keeps its reported hostname.
#[test]
fn no_origin_means_no_machine_segments() {
    use phux_core::host_list::HostKind;
    let f = fixture();
    let hosts = vec![machine("xps", "xps", HostKind::Remote, true, &["s"])];
    let peers = PeerInputs {
        remote_hosts: &hosts,
        ..f.inputs()
    };
    let roster = session_roster(&peers, &[]);
    assert!(roster.iter().all(|r| r.switch_host.is_none()));
    assert!(roster.iter().all(|r| r.host == "mini"));
}
