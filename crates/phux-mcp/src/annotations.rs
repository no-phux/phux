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
//! - `destructiveHint` holds when any method needs `SIGNAL` (it can end a
//!   process or eject a client) or `INPUT` (keystrokes in a live PTY can run
//!   anything). `CREATE` and `BIND` add resources and rewrite bindings,
//!   metadata, and layout: mutating, but not destructive. The rule lives in
//!   `tool_table::dangerous`, the one place L17's kind-table `dangerous`
//!   flag replaces.
//!
//! A tool that runs a local program is destructive; one that only reads
//! local configuration is read-only. A tool with no row is annotated as a
//! destructive write, and the tests make a missing row a failure.
//!
//! [`MethodSpec::mutating`]: phux_protocol::kinds::MethodSpec::mutating

use serde_json::{Value, json};

use crate::tool_table::{self, UNMAPPED};

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

    #[test]
    fn an_unmapped_tool_is_a_destructive_write() {
        let mut tools = json!([{ "name": "phux_not_in_the_table" }]);
        annotate(&mut tools);
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], false);
        assert_eq!(tools[0]["annotations"]["destructiveHint"], true);
    }
}
