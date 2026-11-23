//! Provider words and identifiers are data; unsupported semantics are not guessed.

use super::*;

#[test]
fn renamed_form_fields_and_non_string_options_preserve_provider_values() {
    let field = "provider.profile/v7";
    let offered = json!({"transport": "future-link", "generation": 7});
    let asked = question(
        "form",
        json!({
            "properties": {
                field: {
                    "title": "Choose the current provider profile",
                    "oneOf": [
                        {"const": offered, "title": "Profile supplied by provider"},
                        {"const": 23, "title": "An integer, not a mode name"}
                    ],
                    "provider_annotation": {"revision": 9}
                }
            },
            "required": [field],
            "provider_annotation": "additive metadata"
        }),
    );
    let mut draft = Draft::new(&asked);
    assert!(
        draft.choices.is_empty(),
        "never preselect a provider option"
    );
    assert_eq!(
        draft.options[field][0].label,
        "Profile supplied by provider"
    );
    assert_eq!(draft.options[field][0].id, offered.to_string());
    draft
        .choices
        .insert(field.into(), vec![offered.to_string()]);
    assert_eq!(
        response::build(&draft).unwrap(),
        json!({"values": {field: offered}})
    );
}

#[test]
fn unsupported_type_shapes_never_fall_back_to_string_answers() {
    for declared in [
        json!(["boolean", "null"]),
        json!(["integer", "string"]),
        Value::Null,
        json!(7),
        json!({"new": "type representation"}),
    ] {
        let asked = question(
            "form",
            json!({
                "properties": {"provider-field/v2": {"type": declared}},
                "required": ["provider-field/v2"]
            }),
        );
        let mut draft = Draft::new(&asked);
        typed(&mut draft, "provider-field/v2", "true");
        assert!(
            response::build(&draft).is_err(),
            "unsupported type {declared} was treated as a string"
        );
        assert_eq!(draft.text("provider-field/v2"), "true", "keep the draft");
    }
}

#[test]
fn unconstrained_text_with_new_annotations_remains_supported() {
    assert_eq!(
        response::form_value(
            &json!({"description": "Provider vNext", "provider_annotation": {"new": true}}),
            "the user's literal answer",
            None,
        )
        .unwrap(),
        json!("the user's literal answer")
    );
}

#[test]
fn additive_view_metadata_does_not_invent_capabilities_or_bypass_question_changes() {
    let asked = question("input", json!({"initial_value": "original"}));
    let original = view(vec![asked.clone()], false);
    let mut publication = serde_json::to_value(&original).unwrap();
    publication["provider_annotation"] = json!({"new": true});
    publication["capabilities"]["provider_feature_v2"] = json!(true);
    let decoded: NativeView = serde_json::from_value(publication).unwrap();
    assert!(!decoded.capabilities.questions.is_supported());
    assert!(admit(true, true, Some(&decoded), &asked).is_err());

    let mut changed = view(vec![asked.clone()], true);
    changed.questions[0].fields["initial_value"] = json!("provider changed this request");
    assert_eq!(admit(true, true, Some(&changed), &asked), Err(CLOSED));
}
