//! Socketless, machine-readable installed-binary discovery.

use std::path::Path;
use std::process::ExitCode;

use phux_protocol::caps::ServerFeature;
use phux_protocol::kinds::{self, Carrier, EventSpec, KindSpec, MethodSpec, Verb};
use serde_json::{Value, json};

const CAPABILITIES_SCHEMA_VERSION: u8 = 1;

/// The build-time kind catalog (ADR-0125): the server-level and substrate
/// methods, then one entry per resource kind. The verbs come from the same
/// rows the workload-auth classifier reads.
fn kinds_catalog() -> Value {
    json!({
        "server_methods": methods_json(kinds::SERVER_METHODS),
        "server_events": events_json(kinds::SERVER_EVENTS),
        "substrate": {
            "methods": methods_json(kinds::SUBSTRATE_METHODS),
            "events": events_json(kinds::SUBSTRATE_EVENTS),
        },
        "resource_kinds": kinds::KINDS.iter().map(kind_json).collect::<Vec<_>>(),
    })
}

fn kind_json(kind: &KindSpec) -> Value {
    json!({
        "name": kind.name,
        "tag": kind.kind.as_wire(),
        "gate": kind.gate.map(gate_json),
        "methods": methods_json(kind.methods),
        "events": events_json(kind.events),
        "metadata_keys": kind.metadata_keys,
    })
}

fn methods_json(methods: &[MethodSpec]) -> Vec<Value> {
    methods.iter().map(method_json).collect()
}

fn method_json(method: &MethodSpec) -> Value {
    let rules: Vec<Value> = method
        .rules
        .iter()
        .map(|rule| json!({ "case": rule.case, "requires": rule.requirement_label() }))
        .collect();
    json!({
        "name": method.name,
        "carrier": carrier_json(method.carrier),
        "verbs": method.verbs().iter().map(Verb::name).collect::<Vec<_>>(),
        "mutating": method.mutating(),
        "dangerous": method.dangerous,
        "owner_uds_only": method.owner_uds_only(),
        "gate": method.gate.map(gate_json),
        "shipped": method.shipped,
        "rules": rules,
    })
}

fn carrier_json(carrier: Carrier) -> Value {
    match carrier {
        Carrier::Frame(type_byte) => json!({ "frame": type_byte }),
        Carrier::Command(tag) => json!({ "command": tag }),
        Carrier::Metadata(key) => json!({ "metadata_key": key }),
    }
}

/// A gate as `{ feature, mask }`: `feature` is the name `phux status --json`
/// lists under `features`, and `mask` is the feature's bit value in
/// `HELLO_OK.server_caps.features` (a mask, not a bit index).
fn gate_json(feature: ServerFeature) -> Value {
    json!({
        "feature": crate::feature_names::feature_name(feature),
        "mask": feature as u32,
    })
}

fn events_json(events: &[EventSpec]) -> Vec<Value> {
    events
        .iter()
        .map(|event| json!({ "name": event.name, "tag": event.tag }))
        .collect()
}

fn collect_visible(prefix: &str, meta: &usage::spec::CommandMeta<'_>, paths: &mut Vec<String>) {
    for child in meta.subcommands {
        if child.hide || child.cmd.name == "help" {
            continue;
        }
        let path = format!("{prefix} {}", child.cmd.name);
        paths.push(path.clone());
        collect_visible(&path, child, paths);
    }
}

fn command_paths(meta: &usage::spec::CommandMeta<'_>) -> Vec<String> {
    let mut paths = Vec::new();
    collect_visible("phux", meta, &mut paths);
    paths.sort_unstable();
    paths
}

fn schema_contracts() -> Value {
    json!([
        { "invocation": "phux --capabilities --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux runtime-info --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux ls --json", "schema_version": 3, "kind": "document" },
        { "invocation": "phux snapshot --json", "schema_version": 3, "kind": "document" },
        { "invocation": "phux snapshot --rendered --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux status --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux server --ensure --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux new --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux spawn --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux launch --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux resize --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux ask --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux agent list|show|explain --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux agent wait|prompt|send-keys|answer|start --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux agent session open|emit|log --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux agent log --follow --json", "schema_version": null, "kind": "ndjson", "note": "AgentEventsJsonlV1 records; the record shape is the compatibility contract" },
        { "invocation": "phux config agents --json", "schema_version": 2, "kind": "document" },
        { "invocation": "phux config check|plugins --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux plugin --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux workspace inspect --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux workspace save|restore --json", "schema_version": 2, "kind": "document" },
        { "invocation": "phux worktree --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux host --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux tag --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux pair --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux pair rotate|revoke --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux rec|play --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux logs|doctor --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux update --json", "schema_version": crate::commands::update::DOCUMENT_SCHEMA_VERSION, "kind": "document" },
        { "invocation": "phux channel --json", "schema_version": crate::commands::update::DOCUMENT_SCHEMA_VERSION, "kind": "document" },
        { "invocation": "phux cockpit --json", "schema_version": crate::commands::cockpit::DOCUMENT_SCHEMA_VERSION, "kind": "document" },
        { "invocation": "phux whoami --json", "schema_version": 1, "kind": "document" },
        { "invocation": "phux run --json", "schema_version": null, "kind": "document", "note": "unversioned result" },
        { "invocation": "phux watch --json", "schema_version": null, "kind": "ndjson", "note": "event vocabulary is the compatibility contract" },
        { "invocation": "phux --json failures", "schema_version": 1, "kind": "error" }
    ])
}

fn document(meta: &usage::spec::CommandMeta<'_>, mcp: Option<&Path>) -> Value {
    let protocol = phux_protocol::PROTOCOL_VERSION;
    let available = mcp.is_some();
    json!({
        "schema_version": CAPABILITIES_SCHEMA_VERSION,
        "binary": {
            "name": "phux",
            "version": env!("CARGO_PKG_VERSION"),
            "wire_protocol": format!("{}.{}.{}", protocol.major, protocol.minor, protocol.patch),
        },
        "commands": command_paths(meta),
        "skill": {
            "command": "phux --skill[=SCOPE]",
            "scopes": ["quick", "agent", "terminal", "full"],
            "default_scope": "full"
        },
        "json_contracts": schema_contracts(),
        "kinds": kinds_catalog(),
        "mcp": {
            "available": available,
            "command": mcp,
            "launcher": ["phux", "mcp"],
            "skill_args": ["--skill"],
            "schema_args": ["--schema"],
            "schema_note": "phux mcp --schema is the authoritative tools/list input-schema catalog"
        }
    })
}

pub(crate) fn run() -> ExitCode {
    let mcp = crate::companion::find_live_mcp();
    match serde_json::to_vec_pretty(&document(crate::Cli::spec().root, mcp.as_deref())) {
        Ok(mut rendered) => {
            rendered.push(b'\n');
            crate::output::bytes(&rendered);
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("phux: could not render capabilities: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixture assertions")]
mod tests {
    use super::*;

    #[test]
    fn update_schema_advertisement_matches_the_updater() {
        let contracts = schema_contracts();
        let contracts = contracts.as_array().unwrap();
        for (invocation, schema, kind) in [
            (
                "phux update --json",
                crate::commands::update::DOCUMENT_SCHEMA_VERSION,
                "document",
            ),
            ("phux logs|doctor --json", 1, "document"),
            ("phux --json failures", 1, "error"),
        ] {
            let contract = contracts
                .iter()
                .find(|row| row["invocation"] == invocation)
                .unwrap_or_else(|| panic!("missing contract: {invocation}"));
            assert_eq!(contract["schema_version"], schema, "{invocation}");
            assert_eq!(contract["kind"], kind, "{invocation}");
        }
    }

    fn named<'a>(methods: &'a Value, name: &str) -> &'a Value {
        methods
            .as_array()
            .unwrap()
            .iter()
            .find(|method| method["name"] == name)
            .unwrap_or_else(|| panic!("no catalog method {name}"))
    }

    /// `kinds` is the compiled catalog: every kind with its facet, and every
    /// method with the verbs the workload-auth classifier requires.
    #[test]
    fn capabilities_json_lists_kinds_with_verbs() {
        let doc = document(crate::Cli::spec().root, None);
        let catalog = &doc["kinds"];
        let kinds = catalog["resource_kinds"].as_array().unwrap();
        let names: Vec<&str> = kinds
            .iter()
            .map(|kind| kind["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["TERMINAL", "AGENT_SESSION"]);
        assert_eq!(kinds[0]["tag"], 0);
        assert!(kinds[0]["gate"].is_null());
        assert_eq!(kinds[1]["gate"]["feature"], "resource_kinds");
        assert_eq!(kinds[1]["gate"]["mask"], 0x4000);

        let get_screen = named(&kinds[0]["methods"], "GET_SCREEN");
        assert_eq!(get_screen["verbs"], json!(["OBSERVE"]));
        assert_eq!(get_screen["mutating"], false);
        assert_eq!(get_screen["carrier"], json!({ "command": 7 }));

        let shutdown = named(&catalog["server_methods"], "SHUTDOWN");
        assert_eq!(shutdown["verbs"], json!(["SIGNAL"]));
        assert_eq!(shutdown["mutating"], true);
        assert_eq!(shutdown["owner_uds_only"], true);
        assert_eq!(shutdown["gate"]["feature"], "shutdown");
        assert_eq!(shutdown["gate"]["mask"], 0x100);

        let append = named(&kinds[1]["methods"], "APPEND_RESOURCE_OUTPUT");
        assert_eq!(append["verbs"], json!(["BIND", "INPUT"]));

        let spawn = named(&catalog["substrate"]["methods"], "SPAWN_RESOURCE");
        assert!(spawn["rules"].as_array().unwrap().len() > 1);
        assert!(
            catalog["substrate"]["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["name"] == "pane_closed")
        );
    }

    #[test]
    fn document_is_versioned_sorted_and_hides_plumbing() {
        let doc = document(crate::Cli::spec().root, None);
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["binary"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(doc["binary"]["wire_protocol"], "0.9.0");
        assert_eq!(doc["mcp"]["available"], false);
        assert_eq!(doc["mcp"]["launcher"], json!(["phux", "mcp"]));
        let commands = doc["commands"].as_array().unwrap();
        assert!(
            commands
                .windows(2)
                .all(|pair| { pair[0].as_str().unwrap() < pair[1].as_str().unwrap() })
        );
        assert!(commands.iter().any(|path| path == "phux agent prompt"));
        assert!(commands.iter().any(|path| path == "phux mcp"));
        assert!(!commands.iter().any(|path| path == "phux stdio-bridge"));
        assert!(
            !commands
                .iter()
                .any(|path| path == "phux gen-reference-docs")
        );
        assert!(doc["json_contracts"].as_array().unwrap().iter().any(|row| {
            row["invocation"] == "phux watch --json" && row["schema_version"].is_null()
        }));
    }
}
