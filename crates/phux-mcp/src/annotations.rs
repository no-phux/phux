//! MCP tool annotations (`readOnlyHint`, `destructiveHint`), derived from
//! the kind catalog (ADR-0125) rather than set by hand per tool.
//!
//! Each tool declares what it touches: the catalog methods it can send (by
//! wire name, or by metadata key for a server-interpreted key), or a local
//! effect with no wire method at all. The hints follow from that alone:
//!
//! - `readOnlyHint` holds only when every method is read-only by the
//!   catalog's own conservative rule ([`MethodSpec::mutating`]): a method a
//!   row denies, or the `COMMAND` envelope, counts as mutating, so a denied
//!   write can never hide behind a read-only hint.
//! - `destructiveHint` holds when any method needs `SIGNAL` (it can end a
//!   process or eject a client) or `INPUT` (keystrokes in a live PTY can run
//!   anything). `CREATE` and `BIND` add resources and rewrite bindings,
//!   metadata, and layout: mutating, but not destructive.
//!
//! A tool that runs a local program is destructive; one that only reads
//! local configuration is read-only. A tool with no row here is annotated as
//! a destructive write, and the tests make a missing row a failure.
//!
//! [`MethodSpec::mutating`]: phux_protocol::kinds::MethodSpec::mutating

use phux_protocol::ids::ResourceId;
use phux_protocol::kinds::{self, Verb, Verbs};
use phux_protocol::wire::frame::{
    FrameKind, RESOURCE_TAGS_KEY, SESSION_CREATE_KEY, SESSION_NAME_KEY, Scope, WHOAMI_KEY,
};
use serde_json::{Value, json};

/// What one tool can touch.
#[derive(Debug, Clone, Copy)]
enum Touches {
    /// Catalog methods, by wire name or metadata key.
    Wire(&'static [&'static str]),
    /// Runs a local program (a plugin action): arbitrary effects.
    LocalExec,
    /// Reads local configuration only.
    LocalRead,
}

/// The read methods every targeted tool resolves its selector with.
const RESOLVE: &str = "GET_STATE";

/// A write to an ordinary metadata key: tags, a layout, an agent record.
///
/// Named apart from `SET_METADATA` because that method's rows include the
/// config-reload doorbell, which needs `SIGNAL`; an ordinary write's hints
/// come from the row the classifier gives one ([`kinds::frame_rule`]).
const METADATA_WRITE: &str = "SET_METADATA (ordinary key)";

/// Every tool in the catalog and what it touches.
const TOOLS: &[(&str, Touches)] = &[
    ("phux_ls", Touches::Wire(&[RESOLVE])),
    (
        "phux_snapshot",
        Touches::Wire(&[RESOLVE, "GET_METADATA", "GET_SCREEN"]),
    ),
    ("phux_send_keys", Touches::Wire(&[RESOLVE, "ROUTE_INPUT"])),
    ("phux_paste", Touches::Wire(&[RESOLVE, "ROUTE_INPUT"])),
    (
        "phux_run",
        Touches::Wire(&[RESOLVE, "ROUTE_INPUT", "GET_SCREEN"]),
    ),
    ("phux_wait", Touches::Wire(&[RESOLVE, "GET_SCREEN"])),
    (
        "phux_new",
        Touches::Wire(&[RESOLVE, SESSION_CREATE_KEY, METADATA_WRITE, "GET_METADATA"]),
    ),
    (
        "phux_kill",
        Touches::Wire(&[RESOLVE, "KILL_RESOURCES", "KILL_RESOURCE"]),
    ),
    ("phux_detach", Touches::Wire(&["DETACH_CLIENTS"])),
    (
        "phux_watch",
        Touches::Wire(&[RESOLVE, "SUBSCRIBE_EVENTS", "SUBSCRIBE_METADATA"]),
    ),
    ("phux_ask", Touches::Wire(&[RESOLVE, "REPORT_ASKED"])),
    (
        "phux_launch",
        Touches::Wire(&[
            RESOLVE,
            "SPAWN_RESOURCE",
            METADATA_WRITE,
            "GET_METADATA",
            "KILL_RESOURCE",
        ]),
    ),
    (
        "phux_spawn",
        Touches::Wire(&[
            RESOLVE,
            "SPAWN_RESOURCE",
            METADATA_WRITE,
            "GET_METADATA",
            "KILL_RESOURCE",
        ]),
    ),
    ("phux_signal", Touches::Wire(&[RESOLVE, "SIGNAL_TERMINAL"])),
    (
        "phux_tag",
        Touches::Wire(&[RESOLVE, "GET_METADATA", METADATA_WRITE]),
    ),
    ("phux_rename", Touches::Wire(&[SESSION_NAME_KEY])),
    (
        "phux_insert_pane",
        Touches::Wire(&[RESOLVE, "GET_METADATA", METADATA_WRITE]),
    ),
    (
        "phux_move_pane",
        Touches::Wire(&[RESOLVE, "GET_METADATA", METADATA_WRITE, "MOVE_RESOURCE"]),
    ),
    (
        "phux_swap_pane",
        Touches::Wire(&[RESOLVE, "GET_METADATA", METADATA_WRITE]),
    ),
    (
        "phux_workspace",
        Touches::Wire(&[RESOLVE, "GET_METADATA", METADATA_WRITE, SESSION_CREATE_KEY]),
    ),
    ("phux_plugin_action", Touches::LocalExec),
    ("phux_plugin_workspace", Touches::LocalRead),
    (
        "phux_agent_list",
        Touches::Wire(&[RESOLVE, "GET_METADATA", "GET_SCREEN"]),
    ),
    (
        "phux_agent_show",
        Touches::Wire(&[RESOLVE, "GET_METADATA", "GET_SCREEN"]),
    ),
    (
        "phux_agent_explain",
        Touches::Wire(&[RESOLVE, "GET_METADATA", "GET_SCREEN"]),
    ),
    ("phux_agent_set", Touches::Wire(&[RESOLVE, METADATA_WRITE])),
    (
        "phux_agent_clear",
        Touches::Wire(&[RESOLVE, "DELETE_METADATA"]),
    ),
    (
        "phux_agent_wait",
        Touches::Wire(&[
            RESOLVE,
            "SUBSCRIBE_EVENTS",
            "SUBSCRIBE_METADATA",
            "GET_METADATA",
            "GET_SCREEN",
        ]),
    ),
    (
        "phux_agent_send_keys",
        Touches::Wire(&[RESOLVE, "GET_METADATA", "APPLY_INPUT"]),
    ),
    (
        "phux_agent_prompt",
        Touches::Wire(&[
            RESOLVE,
            "GET_METADATA",
            "APPLY_INPUT",
            "SUBSCRIBE_METADATA",
            "GET_SCREEN",
        ]),
    ),
    (
        "phux_agent_answer",
        Touches::Wire(&[RESOLVE, "GET_METADATA", "APPLY_INPUT"]),
    ),
    (
        "phux_agent_start",
        Touches::Wire(&[
            RESOLVE,
            "GET_METADATA",
            METADATA_WRITE,
            "APPLY_INPUT",
            "SUBSCRIBE_METADATA",
        ]),
    ),
    (
        "phux_agent_session_open",
        Touches::Wire(&[RESOLVE, "SPAWN_RESOURCE"]),
    ),
    (
        "phux_agent_session_close",
        Touches::Wire(&[RESOLVE, "KILL_RESOURCE"]),
    ),
    (
        "phux_agent_emit",
        Touches::Wire(&[RESOLVE, "APPEND_RESOURCE_OUTPUT"]),
    ),
    (
        "phux_agent_log",
        Touches::Wire(&[RESOLVE, "ATTACH_RESOURCE", "DETACH_RESOURCE"]),
    ),
    ("phux_status", Touches::Wire(&[RESOLVE])),
    ("phux_doctor", Touches::Wire(&[RESOLVE])),
    ("phux_whoami", Touches::Wire(&[WHOAMI_KEY])),
    (
        "phux_resource_show",
        Touches::Wire(&[RESOLVE, "GET_TERMINAL_STATE", "GET_METADATA"]),
    ),
    (
        "phux_resource_wait",
        Touches::Wire(&[RESOLVE, "SUBSCRIBE_EVENTS"]),
    ),
    ("phux_resource_methods", Touches::Wire(&[RESOLVE])),
];

/// The two hints one tool carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hints {
    read_only: bool,
    destructive: bool,
}

/// What an unmapped tool is annotated as: the most conservative claim.
const UNMAPPED: Hints = Hints {
    read_only: false,
    destructive: true,
};

/// The hints `touches` implies, or `None` when it names a method the
/// catalog does not have.
fn hints(touches: Touches) -> Option<Hints> {
    match touches {
        Touches::LocalExec => Some(UNMAPPED),
        Touches::LocalRead => Some(Hints {
            read_only: true,
            destructive: false,
        }),
        Touches::Wire(names) => wire_hints(names),
    }
}

fn wire_hints(names: &[&str]) -> Option<Hints> {
    let mut hints = Hints {
        read_only: true,
        destructive: false,
    };
    for name in names {
        let (verbs, mutating) = method_verbs(name)?;
        hints.read_only &= !mutating;
        hints.destructive |= verbs.contains(Verb::Signal) || verbs.contains(Verb::Input);
    }
    Some(hints)
}

/// The verbs `name` can need, and whether it can change state.
fn method_verbs(name: &str) -> Option<(Verbs, bool)> {
    if name == METADATA_WRITE {
        let verbs = kinds::frame_rule(&ordinary_metadata_write()).verb_set();
        // A row that admits no verb is a denial: conservatively a write.
        return Some((verbs, verbs.is_empty() || verbs.mutates()));
    }
    let method = kinds::method_named(name)?;
    Some((method.verbs(), method.mutating()))
}

/// A representative ordinary write: a Terminal's tags.
fn ordinary_metadata_write() -> FrameKind {
    FrameKind::SetMetadata {
        request_id: 0,
        scope: Scope::Resource(ResourceId::local(1)),
        key: RESOURCE_TAGS_KEY.to_owned(),
        value: Vec::new(),
    }
}

fn hints_for(tool: &str) -> Option<Hints> {
    TOOLS
        .iter()
        .find(|(name, _)| *name == tool)
        .and_then(|(_, touches)| hints(*touches))
}

/// Add `annotations` to every tool descriptor in `tools`.
pub(crate) fn annotate(tools: &mut Value) {
    let Value::Array(entries) = tools else {
        return;
    };
    for entry in entries {
        let hints = entry
            .get("name")
            .and_then(Value::as_str)
            .and_then(hints_for)
            .unwrap_or(UNMAPPED);
        entry["annotations"] = json!({
            "readOnlyHint": hints.read_only,
            "destructiveHint": hints.destructive,
        });
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    fn catalog_names() -> Vec<String> {
        crate::tools::catalog()
            .as_array()
            .expect("catalog")
            .iter()
            .map(|tool| tool["name"].as_str().expect("name").to_owned())
            .collect()
    }

    fn annotation(tool: &str) -> Hints {
        let catalog = crate::tools::catalog();
        let entry = catalog
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == tool)
            .unwrap_or_else(|| panic!("no {tool}"));
        Hints {
            read_only: entry["annotations"]["readOnlyHint"].as_bool().unwrap(),
            destructive: entry["annotations"]["destructiveHint"].as_bool().unwrap(),
        }
    }

    /// Every catalog tool has a row, every row names a catalog tool and only
    /// catalog methods, and each descriptor carries exactly the hints the
    /// kind table derives.
    #[test]
    fn annotations_match_kind_table() {
        let names = catalog_names();
        for name in &names {
            let derived = hints_for(name)
                .unwrap_or_else(|| panic!("{name} has no row, or names an unknown method"));
            assert_eq!(annotation(name), derived, "{name}");
        }
        for (tool, _) in TOOLS {
            assert!(names.iter().any(|name| name == tool), "stale row {tool}");
        }
        let mut rows: Vec<&str> = TOOLS.iter().map(|(tool, _)| *tool).collect();
        rows.sort_unstable();
        rows.dedup();
        assert_eq!(rows.len(), TOOLS.len(), "a tool has two rows");
    }

    #[test]
    fn reads_are_read_only_and_signals_and_input_are_destructive() {
        for tool in [
            "phux_ls",
            "phux_snapshot",
            "phux_wait",
            "phux_watch",
            "phux_status",
            "phux_doctor",
            "phux_whoami",
            "phux_agent_list",
            "phux_agent_show",
            "phux_resource_show",
            "phux_resource_wait",
            "phux_resource_methods",
            "phux_plugin_workspace",
        ] {
            assert!(annotation(tool).read_only, "{tool} reads only");
        }
        for tool in [
            "phux_kill",
            "phux_detach",
            "phux_signal",
            "phux_send_keys",
            "phux_paste",
            "phux_run",
            "phux_agent_prompt",
            "phux_agent_send_keys",
            "phux_plugin_action",
        ] {
            let hints = annotation(tool);
            assert!(
                !hints.read_only && hints.destructive,
                "{tool} is destructive"
            );
        }
        for tool in ["phux_new", "phux_tag", "phux_rename", "phux_insert_pane"] {
            let hints = annotation(tool);
            assert!(
                !hints.read_only && !hints.destructive,
                "{tool} writes, additively"
            );
        }
    }

    #[test]
    fn an_unmapped_tool_is_a_destructive_write() {
        let mut tools = json!([{ "name": "phux_not_in_the_table" }]);
        annotate(&mut tools);
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], false);
        assert_eq!(tools[0]["annotations"]["destructiveHint"], true);
    }
}
