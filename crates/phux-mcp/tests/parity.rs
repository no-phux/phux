//! The CLI/MCP parity gate (R4): every agent-facing CLI verb is reachable as
//! an MCP tool, every tool names the verb it mirrors (or why it has none),
//! and every tool's annotations are the kind table's.
//!
//! The table under test is `src/tool_table.rs`, compiled here with `#[path]`
//! — the same file the adapter annotates its catalog from and the generated
//! `docs/reference/parity.md` renders. It is checked against three sources
//! this test does not own:
//!
//! - the live catalog, from `phux-mcp --schema` (the bytes `tools/list`
//!   serves, per `schema_is_exactly_the_live_tools_list_catalog`);
//! - the CLI grammar, from `docs/reference/cli.md`, which the `phux` crate's
//!   freshness test renders from the usage spec and byte-pins, so it is the
//!   grammar and not a hand-kept list;
//! - the agent-facing verbs, from `docs/consumers/agents.md` §7 (the JSON
//!   index), headings and body alike: a backticked token that names a
//!   `phux` invocation path (a leaf, not a group noun like `agent`) is an
//!   agent-facing verb.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a gate that cannot read its inputs must fail loudly"
)]

#[allow(
    clippy::redundant_pub_crate,
    reason = "the table is written for the adapter crate, where pub(crate) is load-bearing"
)]
#[path = "../src/tool_table.rs"]
mod tool_table;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use tool_table::{CLI_ONLY, DESTRUCTIVE_SOURCE, Exec, Surface, TOOLS};

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("reading {relative}: {err}"))
}

/// The live tool catalog, exactly as `tools/list` serves it.
fn catalog() -> Vec<Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_phux-mcp"))
        .arg("--schema")
        .env_remove("PHUX_SOCKET")
        .output()
        .expect("run phux-mcp --schema");
    assert!(output.status.success(), "phux-mcp --schema failed");
    let catalog: Value = serde_json::from_slice(&output.stdout).expect("schema is JSON");
    catalog.as_array().expect("catalog is an array").clone()
}

/// Every `phux` invocation path in the generated CLI reference, without the
/// leading `phux ` (`ls`, `agent wait`, `resource show`).
fn cli_verbs() -> BTreeSet<String> {
    repo_file("docs/reference/cli.md")
        .lines()
        .filter_map(|line| line.strip_prefix("## `phux ")?.strip_suffix('`'))
        .map(str::to_owned)
        .collect()
}

/// The agent-facing verbs `docs/consumers/agents.md` §7 indexes: every
/// backticked token in the section, headings and body, that names a CLI
/// invocation path, with a leading `phux ` and any flag suffix
/// (`watch --json`) dropped. A group noun (`agent`, `host`, `resource`) is a
/// parent of other paths, not an action with a document, and is skipped.
fn agent_facing_verbs(cli: &BTreeSet<String>) -> BTreeSet<String> {
    let agents = repo_file("docs/consumers/agents.md");
    let start = agents
        .find("\n## 7. JSON index")
        .expect("agents.md has the JSON index");
    let end = agents[start + 1..]
        .find("\n## ")
        .map_or(agents.len(), |offset| start + 1 + offset);
    let is_group = |path: &str| {
        cli.iter()
            .any(|other| other.starts_with(&format!("{path} ")))
    };
    agents[start..end]
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|token| !token.contains('\n'))
        .map(|token| {
            let token = token.split(" --").next().unwrap_or(token).trim();
            token.strip_prefix("phux ").unwrap_or(token).to_owned()
        })
        .filter(|token| cli.contains(token) && !is_group(token))
        .collect()
}

fn tool_verbs() -> BTreeSet<&'static str> {
    TOOLS
        .iter()
        .filter_map(|row| match row.surface {
            Surface::Cli(verb) => Some(verb),
            Surface::AutomationOnly(_) => None,
        })
        .collect()
}

/// (a) Every agent-facing CLI verb in the JSON index has a tool, or an
/// explicit row saying why not.
#[test]
fn every_agent_facing_cli_verb_has_a_tool() {
    let cli = cli_verbs();
    let agent_facing = agent_facing_verbs(&cli);
    assert!(
        agent_facing.len() >= 25,
        "the JSON index parse found too few verbs to be meaningful ({} found): {agent_facing:?}",
        agent_facing.len()
    );
    let tools = tool_verbs();
    for verb in &agent_facing {
        let exempt = CLI_ONLY.iter().any(|(name, _)| name == verb);
        assert!(
            tools.contains(verb.as_str()) || exempt,
            "`phux {verb}` is indexed in agents.md §7 but no MCP tool mirrors it; add the tool, \
             or a CLI_ONLY row in crates/phux-mcp/src/tool_table.rs saying why not"
        );
        assert!(
            !(tools.contains(verb.as_str()) && exempt),
            "`phux {verb}` has a tool and a CLI_ONLY row; drop the row"
        );
    }
    for (verb, reason) in CLI_ONLY {
        assert!(
            cli.contains(*verb),
            "CLI_ONLY names `phux {verb}`, not a CLI verb"
        );
        assert!(!reason.is_empty(), "CLI_ONLY `{verb}` needs a reason");
    }
}

/// (b) The table and the catalog agree tool for tool, and every tool names
/// a real CLI verb or says why it has none; every residue tool says why it
/// still runs the CLI.
#[test]
fn every_tool_names_its_verb_or_is_automation_only() {
    let cli = cli_verbs();
    let served: BTreeSet<String> = catalog()
        .iter()
        .map(|tool| tool["name"].as_str().expect("name").to_owned())
        .collect();
    let rows: BTreeSet<String> = TOOLS.iter().map(|row| row.name.to_owned()).collect();
    assert_eq!(rows.len(), TOOLS.len(), "a tool has two rows");
    assert_eq!(
        served, rows,
        "the tool table and the live catalog disagree; add or remove the row in \
         crates/phux-mcp/src/tool_table.rs"
    );
    for row in TOOLS {
        match row.surface {
            Surface::Cli(verb) => assert!(
                cli.contains(verb),
                "{} mirrors `phux {verb}`, which is not in the CLI grammar",
                row.name
            ),
            Surface::AutomationOnly(reason) => {
                assert!(!reason.is_empty(), "{} needs a reason", row.name);
            }
        }
        if let Exec::Cli(reason) = row.exec {
            assert!(
                !reason.is_empty(),
                "residue tool {} needs a reason",
                row.name
            );
            assert!(
                matches!(row.surface, Surface::Cli(_)),
                "{} runs the CLI, so it must name the verb it runs",
                row.name
            );
        }
    }
}

/// (c) Each tool's served annotations are the kind table's: `readOnlyHint`
/// is "no method it can send is mutating" by `MethodSpec::mutating`, and
/// `destructiveHint` is the kind table's `dangerous` flag or an `INPUT`
/// method (ADR-0128).
///
/// The hint comparison is mostly self-referential: the adapter annotates
/// its catalog through `tool_table::hints_for`, which reads the same table
/// `hints` does here. The independent check is the loop below, which asks
/// the kind table directly (`method_named(..).mutating()`) whether any
/// method a read-only tool names can change state.
#[test]
fn tool_annotations_equal_the_kind_table() {
    assert!(DESTRUCTIVE_SOURCE.contains("MethodSpec.dangerous"));
    for tool in catalog() {
        let name = tool["name"].as_str().expect("name");
        let row = tool_table::row(name).unwrap_or_else(|| panic!("{name} has no row"));
        let hints = tool_table::hints(row.touches)
            .unwrap_or_else(|| panic!("{name} names a method the kind table does not have"));
        let annotations = &tool["annotations"];
        assert_eq!(
            annotations["readOnlyHint"],
            Value::Bool(hints.read_only),
            "{name}: readOnlyHint is not the kind table's `mutating`"
        );
        assert_eq!(
            annotations["destructiveHint"],
            Value::Bool(hints.destructive),
            "{name}: destructiveHint is not {DESTRUCTIVE_SOURCE}"
        );
        if !hints.read_only {
            continue;
        }
        if let tool_table::Touches::Wire(methods) = row.touches {
            for method in methods {
                let spec = phux_protocol::kinds::method_named(method)
                    .unwrap_or_else(|| panic!("{name}: {method} is not in the kind table"));
                assert!(!spec.mutating(), "{name} is read-only but {method} mutates");
            }
        }
    }
}
