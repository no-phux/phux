//! The kind dispatch, its refusals, and the text a document wait matches.

use super::*;

fn at(kind: ResourceKind) -> Resolved {
    Resolved {
        id: ResourceId::local(5),
        kind,
    }
}

const ALL: [ResourceKind; 6] = [
    ResourceKind::Terminal,
    ResourceKind::AgentSession,
    ResourceKind::AccessibilityTree,
    ResourceKind::PixelSurface,
    ResourceKind::Document,
    ResourceKind::Unknown { tag: 200 },
];

#[test]
fn each_verb_routes_each_kind_from_the_catalog() {
    use Route::{Document as Doc, Terminal};
    let expect = |verb: Verb| -> [Option<Route>; 6] {
        match verb {
            Verb::Snapshot => [
                Some(Terminal),
                None,
                Some(Doc(Kind::Tree)),
                Some(Doc(Kind::Surface)),
                Some(Doc(Kind::Document)),
                None,
            ],
            Verb::Wait => [
                Some(Terminal),
                None,
                Some(Doc(Kind::Tree)),
                None,
                Some(Doc(Kind::Document)),
                None,
            ],
            Verb::SendKeys => [Some(Terminal), None, None, None, None, None],
        }
    };
    for verb in [Verb::Snapshot, Verb::Wait, Verb::SendKeys] {
        for (kind, want) in ALL.into_iter().zip(expect(verb)) {
            let got = route(verb, &at(kind));
            assert_eq!(got.as_ref().ok().copied(), want, "{verb:?} {kind:?}");
            if let Err(refusal) = got {
                assert_eq!((refusal.kind, refusal.verb), (kind, verb));
            }
        }
    }
}

#[test]
fn a_refusal_names_the_verb_the_resource_and_its_kind_and_what_to_do() {
    let keys = route(Verb::SendKeys, &at(ResourceKind::AccessibilityTree)).unwrap_err();
    assert_eq!(
        keys.to_string(),
        "`send-keys` does not apply to @5, an accessibility tree"
    );
    assert!(
        keys.remedy().contains("phux agent page key"),
        "{}",
        keys.remedy()
    );

    let surface = route(Verb::Wait, &at(ResourceKind::PixelSurface)).unwrap_err();
    assert!(
        surface.remedy().contains("--out FILE"),
        "{}",
        surface.remedy()
    );

    let session = route(Verb::Snapshot, &at(ResourceKind::AgentSession)).unwrap_err();
    assert!(session.to_string().ends_with("an agent session"));
    assert!(session.remedy().contains("phux agent log"));

    let unknown = route(Verb::Snapshot, &at(ResourceKind::Unknown { tag: 200 })).unwrap_err();
    assert!(unknown.to_string().contains("unknown(200)"), "{unknown}");
    assert!(unknown.remedy().contains("update phux"));
}

#[test]
fn terminal_options_are_refused_only_off_the_terminal_route() {
    let tree = at(ResourceKind::AccessibilityTree);
    let options = [("--cells", false), ("--tail", true), ("--unwrap", true)];
    let refusal = terminal_only(&tree, Route::Document(Kind::Tree), &options).unwrap_err();
    assert_eq!(refusal.option, "--tail", "the first one given");
    assert_eq!(
        refusal.to_string(),
        "`--tail` applies to a terminal only, and @5 is an accessibility tree"
    );
    assert!(terminal_only(&at(ResourceKind::Terminal), Route::Terminal, &options).is_ok());
    assert!(terminal_only(&tree, Route::Document(Kind::Tree), &[("--tail", false)]).is_ok());
}

fn doc(content_type: Option<&str>, bytes: &[u8]) -> Document {
    Document {
        seq: 3,
        bytes: bytes.to_vec(),
        content_type: content_type.map(str::to_owned),
    }
}

#[test]
fn a_tree_and_a_text_document_have_text_and_pixels_do_not() {
    let json = doc(None, br#"{"title":"Save"}"#);
    assert_eq!(
        text_of(Kind::Tree, &json).as_deref(),
        Some(r#"{"title":"Save"}"#)
    );
    assert_eq!(text_of(Kind::Surface, &json), None);
    let markdown = doc(Some("text/markdown"), b"# done");
    assert_eq!(
        text_of(Kind::Document, &markdown).as_deref(),
        Some("# done")
    );
    assert_eq!(
        text_of(Kind::Document, &doc(Some("image/png"), b"\x89PNG")),
        None
    );
}

#[test]
fn a_document_reads_back_with_its_text_or_without_one() {
    let markdown = document_json(&doc(Some("text/markdown"), b"# done"));
    assert_eq!(markdown["text"], "# done");
    assert_eq!(markdown["bytes"], 6);
    assert_eq!(markdown["content_type"], "text/markdown");
    let binary = document_json(&doc(Some("application/octet-stream"), b"\0\x01"));
    assert!(binary["text"].is_null());
    assert!(binary["format"].is_null());
}

#[test]
fn a_text_match_is_a_substring_of_the_whole_text_or_a_regex_on_one_line() {
    let contains = Condition::Contains("\"b\":".to_owned());
    let matcher = TextMatch::of(&contains).unwrap();
    assert!(matcher.is_match("{\"a\":1,\n\"b\":2}"));
    let pattern = Condition::Matches("^## Done$".parse().unwrap());
    let matcher = TextMatch::of(&pattern).unwrap();
    assert!(matcher.is_match("# Plan\n## Done\nrest"));
    assert!(!matcher.is_match("# Plan ## Done"));
    assert!(TextMatch::of(&Condition::Idle(std::time::Duration::from_millis(5))).is_none());
}
