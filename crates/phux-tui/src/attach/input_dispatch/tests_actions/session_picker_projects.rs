//! Project tags regroup the session picker without losing hosts or details.

use super::*;

/// phux-jx32h: project tags regroup only the attached host. Satellites,
/// other machines, and unreachable hosts stay, and each local row keeps the
/// untagged picker's details. Duplicate names join by session id.
#[test]
fn session_picker_project_groups_keep_hosts_and_row_details() {
    let tagged = mixed_project_picker(true);
    let plain = mixed_project_picker(false);
    assert_eq!(picker_labels(&tagged), TAGGED_PICKER_LABELS);
    assert_eq!(picker_labels(&plain), PLAIN_PICKER_LABELS);

    let local = attached_picker_row(&tagged, 1);
    assert_eq!(local.secondary, None);
    assert_eq!(local.secondary, attached_picker_row(&plain, 1).secondary);
    assert_eq!(local.action, attached_picker_row(&plain, 1).action);
    assert!(local.indented);
    let focused = attached_picker_row(&tagged, 2);
    assert_eq!(focused.secondary, None);
    assert_eq!(focused.label, "web");
    assert!(!focused.action.args.contains_key("host"));
    let duplicate = attached_picker_row(&tagged, 4);
    assert_eq!(duplicate.label, "api");
    assert_eq!(duplicate.secondary, None);
    assert_eq!(duplicate.action.args.get("id"), Some(&Value::Integer(4)));
    assert_eq!(
        attached_picker_row(&tagged, 3).secondary,
        attached_picker_row(&plain, 3).secondary
    );
    assert_remote_sections_match(&tagged, &plain);
}

fn assert_remote_sections_match(
    tagged: &[crate::render::overlay::SelectItem],
    plain: &[crate::render::overlay::SelectItem],
) {
    let tagged_tail: Vec<_> = tagged
        .iter()
        .skip_while(|item| item.label != "edge")
        .collect();
    let plain_tail: Vec<_> = plain
        .iter()
        .skip_while(|item| item.label != "edge")
        .collect();
    assert_eq!(tagged_tail.len(), plain_tail.len());
    for (tagged_row, plain_row) in tagged_tail.iter().zip(&plain_tail) {
        assert_eq!(tagged_row.label, plain_row.label);
        assert_eq!(tagged_row.secondary, plain_row.secondary);
        assert_eq!(tagged_row.action, plain_row.action);
        assert_eq!(tagged_row.is_header(), plain_row.is_header());
    }
    let edge = tagged
        .iter()
        .find(|item| item.label == "build" && item.action.action == "switch-session")
        .expect("satellite build");
    assert_eq!(
        edge.action.args.get("host"),
        Some(&Value::String("edge".to_owned()))
    );
    assert_eq!(edge.secondary.as_deref(), Some("on edge"));
    let mini = tagged
        .iter()
        .find(|item| item.action.action == "switch-host" && item.label == "phall")
        .expect("machine phall");
    assert_eq!(
        mini.action.args.get("host"),
        Some(&Value::String("mini".to_owned()))
    );
    assert_eq!(mini.secondary.as_deref(), Some("on mini, 1 needs you"));
    assert!(
        tagged
            .iter()
            .any(|item| item.is_header() && item.label == "down - unreachable")
    );
    assert!(
        tagged
            .iter()
            .any(|item| item.is_header() && item.label == "xps - unreachable")
    );
}
