//! JSON Schema and structured-content contract checks.
use serde_json::{Value, json};
pub(crate) fn validate_output(name: &str, value: &Value) {
    let catalog = crate::tools::catalog();
    let tool = catalog
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap();
    let validator = jsonschema::validator_for(&tool["outputSchema"]).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "{name}: {errors:?}; document={value}");
}
#[test]
fn every_tool_publishes_valid_input_and_output_schemas() {
    let catalog = crate::tools::catalog();
    let outputs = crate::contracts::output_schemas();
    assert_eq!(
        catalog.as_array().unwrap().len(),
        outputs.as_object().unwrap().len()
    );
    for tool in catalog.as_array().unwrap() {
        for key in ["inputSchema", "outputSchema"] {
            assert_eq!(tool[key]["type"], "object", "{} {key}", tool["name"]);
            jsonschema::validator_for(&tool[key])
                .unwrap_or_else(|error| panic!("{} {key}: {error}", tool["name"]));
        }
    }
}
#[test]
fn exact_id_alias_satisfies_schema_without_selector_and_rejects_ambiguity() {
    let catalog = crate::tools::catalog();
    for tool in catalog.as_array().unwrap() {
        if tool["inputSchema"]["properties"].get("target").is_none() {
            continue;
        }
        assert!(
            tool["inputSchema"]["properties"]
                .get("resource_id")
                .is_some()
        );
    }
    let tool = catalog
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "phux_paste")
        .unwrap();
    let validator = jsonschema::validator_for(&tool["inputSchema"]).unwrap();
    assert!(validator.is_valid(&json!({"resource_id":"host/@7", "text":"hi"})));
    assert!(validator.is_valid(&json!({"target":"work", "text":"hi"})));
    assert!(!validator.is_valid(&json!({"text":"hi"})));
    assert!(!validator.is_valid(&json!({"resource_id":"@7", "text":"hi", "background":true})));
    assert!(!validator.is_valid(&json!({"resource_id":"@7", "target":"work", "text":"hi"})));
    for invalid in [
        json!({"resource_id":"work"}),
        json!({"resource_id":"#tag"}),
        json!({"resource_id":7}),
        json!({"resource_id":"@7","target":"@8"}),
    ] {
        assert!(crate::contracts::normalize_target(invalid).is_err());
    }
}
#[test]
fn structured_content_and_text_are_the_same_document_for_success_and_errors() {
    let error = crate::tools::contract_error("no_server", "missing", "start a test server", 1);
    for (document, is_error) in [(json!({"sessions":[]}), false), (error.document(), true)] {
        let content = crate::tool_content(&document, is_error);
        assert_eq!(content["structuredContent"], document);
        assert_eq!(content["isError"], is_error);
        let text: Value =
            serde_json::from_str(content["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, document);
    }
}
#[tokio::test]
async fn successful_resource_inspection_allows_refused_metadata_reads() {
    use phux_client::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::ids::{ResourceId, ResourceKind, SessionId, WindowId};
    use phux_protocol::wire::frame::ErrorCode;
    use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("inspect.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let state = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(7))
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(7), WindowId::new(1), 0, 0)
                .with_kind(ResourceKind::AgentSession),
        ]);
    let spec = ScriptSpec::new()
        .state(state)
        .refuse_metadata(ErrorCode::PermissionDenied, "permission denied");
    let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
    let document = crate::resource_tools::call(
        "phux_resource_show",
        &json!({"target":"@7","socket":socket}),
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert!(document["tags"].is_null());
    validate_output("phux_resource_show", &document);
}

#[test]
fn project_contract_advertises_the_actual_repository_field() {
    let doc = json!({"schema_version":1,"repo":{"path":"/checkout","head":null,"branch":null,"detached":true},"worktrees":[]});
    validate_output("phux_project_inspect", &doc);
    let outputs = crate::contracts::output_schemas();
    assert!(
        outputs["phux_project_inspect"]["properties"]
            .get("repository")
            .is_none()
    );
}

#[test]
fn resource_receipts_expose_canonical_local_and_satellite_ids() {
    for (doc, expected) in [
        (json!({"terminal_id":7}), "@7"),
        (
            json!({"terminal_id":7,"satellite":"region/@build"}),
            "region/@build/@7",
        ),
        (json!({"resource":"host/@7"}), "host/@7"),
    ] {
        assert_eq!(
            crate::contracts::result_document(doc)["resource_id"],
            expected
        );
    }
}

#[test]
fn initialization_negotiates_supported_older_hosts() {
    for version in ["2024-11-05", "2025-03-26", crate::MCP_PROTOCOL_VERSION] {
        assert_eq!(
            crate::initialize_result(Some(&json!({"protocolVersion":version})))["protocolVersion"],
            version
        );
    }
    assert_eq!(
        crate::initialize_result(Some(&json!({"protocolVersion":"future"})))["protocolVersion"],
        crate::MCP_PROTOCOL_VERSION
    );
}

#[tokio::test]
async fn exact_resource_id_does_not_require_an_inventory_connection() {
    let selector = crate::tools::parse_selector("host/@7").unwrap();
    let id = crate::tools::resolve_selector(
        std::path::Path::new("/nonexistent/no-inventory.sock"),
        &selector,
        true,
    )
    .await
    .unwrap();
    assert_eq!(phux_client::selector::format_terminal_id(&id), "host/@7");
}
