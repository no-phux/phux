//! MCP tool annotations (`readOnlyHint`, `destructiveHint`), derived from
//! the kind catalog (ADR-0125) rather than set by hand per tool.
//!
//! Each tool's row in [`crate::tool_table`] declares what it touches: the
//! catalog methods it can send (by wire name, or by metadata key for a
//! server-interpreted key), or a local effect with no wire method at all.
//! The hints follow from that alone:
//!
//! - `readOnlyHint` holds only when every method is read-only by the
//!   catalog's own conservative rule ([`MethodSpec::mutating`]): a method a
//!   row denies, or the `COMMAND` envelope, counts as mutating, so a denied
//!   write can never hide behind a read-only hint.
//! - `destructiveHint` holds when any method is `MethodSpec::dangerous` (it
//!   can end a process, eject a client, stop the server, or release a held
//!   action; ADR-0128) or needs `INPUT` (keystrokes in a live PTY can run
//!   anything). `CREATE` and `BIND` add resources and rewrite bindings,
//!   metadata, and layout: mutating, but not destructive. The rule lives in
//!   `tool_table::dangerous`.
//!
//! The same catalog fact drives the one `confirm` check every tool shares
//! ([`require_confirmation`]): a tool whose own action is a dangerous method
//! needs `confirm: true`, unless the catalog says this call's payload is the
//! reversible kind (`freeze`, `resume`) or releases nothing (`deny`).
//!
//! A tool that runs a local program is destructive; one that only reads
//! local configuration is read-only. A tool with no row is annotated as a
//! destructive write, and the tests make a missing row a failure.
//!
//! [`MethodSpec::mutating`]: phux_protocol::kinds::MethodSpec::mutating

use phux_protocol::ids::ResourceId;
use phux_protocol::kinds;
use phux_protocol::wire::frame::{
    APPROVAL_DECIDE_KEY_PREFIX, Command, FrameKind, Scope, TerminalSignal,
};
use serde_json::{Value, json};

use crate::tool_table::{self, Row, Touches, UNMAPPED};
use crate::tools::ToolError;

/// Which part of a call picks its payload, where the catalog's danger
/// depends on it.
#[derive(Debug, Clone, Copy)]
enum Payload {
    /// Every call is the same instance.
    Whole,
    /// The `signal` argument: `freeze` and `resume` are the reversible brake.
    Signal,
    /// The `decision` argument: `deny` releases nothing.
    Decision,
}

/// Tools that send a dangerous method only as the teardown their own verb
/// names (ADR-0128): the call is the consent, so they never ask again.
const TEARDOWN: &[(&str, &str)] = &[
    (
        "phux_launch",
        "a launch that cannot place its pane rolls back the pane it just spawned",
    ),
    (
        "phux_spawn",
        "a spawn that cannot place its pane rolls back the pane it just spawned",
    ),
    (
        "phux_agent_session_close",
        "closing the named agent session is the tool's whole act",
    ),
];

/// Which argument picks a tool's instance, where the catalog's danger
/// depends on it.
fn payload_of(tool: &str) -> Payload {
    match tool {
        "phux_signal" => Payload::Signal,
        "phux_approve" => Payload::Decision,
        _ => Payload::Whole,
    }
}

/// The dangerous method a tool sends as its own act, read from its row:
/// the first method it touches that the kind table marks dangerous, unless
/// the tool is a named teardown. A new tool that sends a dangerous method
/// asks for `confirm` without anyone listing it.
fn confirmed_method(row: &Row) -> Option<&'static str> {
    if TEARDOWN.iter().any(|(name, _)| *name == row.name) {
        return None;
    }
    let Touches::Wire(names) = row.touches else {
        return None;
    };
    names
        .iter()
        .copied()
        .find(|name| kinds::method_named(name).is_some_and(|method| method.dangerous))
}

/// The one `confirm` check: a call whose instance the catalog marks
/// dangerous needs `confirm: true`.
///
/// # Errors
///
/// A [`ToolError`] naming the tool when `confirm` is absent or not `true`.
pub(crate) fn require_confirmation(tool: &str, args: &Value) -> Result<(), ToolError> {
    let Some(method) = tool_table::row(tool).and_then(confirmed_method) else {
        return Ok(());
    };
    if !instance_is_dangerous(method, payload_of(tool), args)
        || args.get("confirm") == Some(&Value::Bool(true))
    {
        return Ok(());
    }
    Err(ToolError::new(format!(
        "{tool} is destructive; pass `confirm: true`"
    )))
}

/// Whether this call's instance of `method` is dangerous, asked of the
/// catalog with the command or frame the call would send. An argument that
/// does not parse is treated as dangerous; the tool refuses it anyway.
fn instance_is_dangerous(method: &str, payload: Payload, args: &Value) -> bool {
    if !kinds::method_named(method).is_none_or(|spec| spec.dangerous) {
        return false;
    }
    match payload {
        Payload::Whole => true,
        Payload::Signal => signal_arg(args).is_none_or(|signal| {
            kinds::command_is_dangerous(&Command::SignalTerminal {
                terminal_id: ResourceId::local(1),
                signal,
                operation_id: None,
            })
        }),
        Payload::Decision => kinds::frame_is_dangerous(&decision_frame(args)),
    }
}

fn signal_arg(args: &Value) -> Option<TerminalSignal> {
    match args.get("signal").and_then(Value::as_str)? {
        "interrupt" => Some(TerminalSignal::Interrupt),
        "freeze" => Some(TerminalSignal::Freeze),
        "resume" => Some(TerminalSignal::Resume),
        "terminate" => Some(TerminalSignal::Terminate),
        "kill" => Some(TerminalSignal::Kill),
        _ => None,
    }
}

/// The decision frame a `phux_approve` call sends, on a placeholder id.
fn decision_frame(args: &Value) -> FrameKind {
    let decision = args
        .get("decision")
        .and_then(Value::as_str)
        .unwrap_or("approve");
    FrameKind::SetMetadata {
        request_id: 0,
        scope: Scope::Global,
        key: format!("{APPROVAL_DECIDE_KEY_PREFIX}{:032x}", 1),
        value: decision.as_bytes().to_vec(),
    }
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
            .and_then(tool_table::hints_for)
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
    use crate::tool_table::{Hints, TOOLS, hints_for};

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
        for row in TOOLS {
            assert!(
                names.iter().any(|name| name == row.name),
                "stale row {}",
                row.name
            );
        }
        let mut rows: Vec<&str> = TOOLS.iter().map(|row| row.name).collect();
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

    /// The dangerous methods, pinned by name (ADR-0128).
    const DANGEROUS: [&str; 11] = [
        "KILL_RESOURCE",
        "KILL_RESOURCE_IF",
        "KILL_RESOURCES",
        "CLOSE_TAB_RESOURCES",
        "SIGNAL_TERMINAL",
        "DETACH_CLIENTS",
        "SHUTDOWN",
        "OPEN_LISTENER",
        "UPGRADE",
        phux_protocol::wire::frame::CONFIG_RELOAD_KEY,
        APPROVAL_DECIDE_KEY_PREFIX,
    ];

    /// Each dangerous method and the CLI verb that asks before sending it.
    const CLI_GATES: [(&str, &str); 6] = [
        ("KILL_RESOURCE", "kill"),
        ("KILL_RESOURCE_IF", "kill"),
        ("KILL_RESOURCES", "kill"),
        ("SIGNAL_TERMINAL", "signal"),
        ("DETACH_CLIENTS", "detach"),
        (APPROVAL_DECIDE_KEY_PREFIX, "approve"),
    ];

    /// The dangerous methods no CLI verb asks before, and why.
    const CLI_UNGATED: [(&str, &str); 5] = [
        (
            "CLOSE_TAB_RESOURCES",
            "no CLI verb or MCP tool sends it; a native client's explicit Close Tab is the act",
        ),
        (
            "SHUTDOWN",
            "`phux kill --server` is the owner socket's own service stop; no scoped grant reaches it",
        ),
        (
            "UPGRADE",
            "`phux upgrade` re-execs in place and keeps every session",
        ),
        (
            "OPEN_LISTENER",
            "no verb sends it on request; `phux attach --ssh` opens a door for its own attach",
        ),
        (
            phux_protocol::wire::frame::CONFIG_RELOAD_KEY,
            "`phux config reload` makes consumers re-read their own configuration",
        ),
    ];

    /// The section for `phux VERB` in the generated CLI reference.
    fn cli_reference(verb: &str) -> String {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/reference/cli.md");
        let doc = std::fs::read_to_string(&path).expect("docs/reference/cli.md");
        let heading = format!("\n## `phux {verb}`\n");
        let start = doc
            .find(&heading)
            .unwrap_or_else(|| panic!("no `phux {verb}` in the CLI reference"));
        let rest = &doc[start + heading.len()..];
        rest[..rest.find("\n## ").unwrap_or(rest.len())].to_owned()
    }

    /// One fact, three consumers (ADR-0128): the catalog's dangerous set,
    /// MCP's `destructiveHint` and `confirm`, and the CLI's `--yes`.
    #[test]
    fn dangerous_rows_match_the_mcp_destructive_hints_and_cli_yes_flags() {
        let dangerous: std::collections::BTreeSet<&str> = kinds::methods()
            .filter(|method| method.dangerous)
            .map(|method| method.name)
            .collect();
        assert_eq!(dangerous, DANGEROUS.into_iter().collect());

        let confirming: Vec<&str> = TOOLS
            .iter()
            .filter(|row| confirmed_method(row).is_some())
            .map(|row| row.name)
            .collect();
        assert_eq!(
            confirming,
            ["phux_kill", "phux_detach", "phux_signal", "phux_approve"],
            "the tools that confirm are derived from their rows"
        );
        for tool in &confirming {
            assert!(annotation(tool).destructive, "{tool} is destructive");
            assert!(require_confirmation(tool, &json!({})).is_err(), "{tool}");
            assert!(require_confirmation(tool, &json!({ "confirm": true })).is_ok());
        }
        for tool in catalog_names() {
            assert!(
                confirming.contains(&tool.as_str())
                    || require_confirmation(&tool, &json!({})).is_ok(),
                "{tool}"
            );
        }
        for (tool, reason) in TEARDOWN {
            let row = tool_table::row(tool).unwrap_or_else(|| panic!("stale teardown {tool}"));
            assert!(!reason.is_empty());
            let Touches::Wire(names) = row.touches else {
                panic!("{tool} sends nothing");
            };
            assert!(
                names
                    .iter()
                    .any(|name| kinds::method_named(name).is_some_and(|m| m.dangerous)),
                "{tool} is listed as a teardown but sends no dangerous method"
            );
        }
        let brake = json!({ "signal": "freeze" });
        assert!(require_confirmation("phux_signal", &brake).is_ok());
        let denial = json!({ "decision": "deny" });
        assert!(require_confirmation("phux_approve", &denial).is_ok());

        let gated: std::collections::BTreeSet<&str> =
            CLI_GATES.iter().map(|(method, _)| *method).collect();
        let ungated: std::collections::BTreeSet<&str> =
            CLI_UNGATED.iter().map(|(method, _)| *method).collect();
        assert!(gated.is_disjoint(&ungated));
        assert_eq!(
            gated
                .union(&ungated)
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            dangerous,
            "every dangerous method is gated by a CLI verb or named ungated"
        );
        for (method, verb) in CLI_GATES {
            assert!(
                cli_reference(verb).contains("--yes"),
                "`phux {verb}` sends {method} and must take --yes"
            );
        }
    }

    /// A tool added later that sends `KILL_RESOURCE` confirms without being
    /// listed anywhere.
    #[test]
    fn a_new_tool_sending_a_dangerous_method_must_confirm() {
        let row = Row {
            name: "phux_hypothetical_reaper",
            surface: tool_table::Surface::AutomationOnly("test"),
            exec: tool_table::Exec::InProcess,
            touches: Touches::Wire(&["GET_STATE", "KILL_RESOURCE"]),
        };
        assert_eq!(confirmed_method(&row), Some("KILL_RESOURCE"));
    }

    #[test]
    fn an_unmapped_tool_is_a_destructive_write() {
        let mut tools = json!([{ "name": "phux_not_in_the_table" }]);
        annotate(&mut tools);
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], false);
        assert_eq!(tools[0]["annotations"]["destructiveHint"], true);
    }
}
