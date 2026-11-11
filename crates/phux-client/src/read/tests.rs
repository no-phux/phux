//! The kind dispatch, its refusals, and the text a document wait matches.

use super::*;

fn at(kind: ResourceKind) -> Resolved {
    Resolved {
        id: ResourceId::local(5),
        kind,
        parent: None,
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
            _ => [Some(Terminal), None, None, None, None, None],
        }
    };
    for verb in Verb::ALL {
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
fn each_verb_names_a_method_only_the_terminal_facet_carries() {
    for verb in Verb::ALL {
        let carriers: Vec<_> = kinds::KINDS
            .iter()
            .filter(|spec| {
                spec.methods
                    .iter()
                    .any(|method| method.name == verb.terminal_method())
            })
            .map(|spec| spec.kind)
            .collect();
        assert_eq!(carriers, [ResourceKind::Terminal], "{verb:?}");
    }
}

#[test]
fn a_terminal_verb_refuses_every_other_kind_up_front() {
    for verb in Verb::ALL {
        assert_eq!(
            require_terminal(verb, &at(ResourceKind::Terminal)),
            Ok(()),
            "{verb:?}"
        );
        for kind in &ALL[1..] {
            let refusal = require_terminal(verb, &at(*kind)).unwrap_err();
            assert_eq!((refusal.kind, refusal.verb), (*kind, verb));
        }
    }
    // A verb that types into a page's tree names its producer's intents; a
    // typed document takes none; any other verb names a Terminal.
    let paste = require_terminal(Verb::Paste, &at(ResourceKind::AccessibilityTree)).unwrap_err();
    assert_eq!(
        paste.to_string(),
        "`paste` does not apply to @5, an accessibility tree"
    );
    assert!(
        paste.remedy().contains("phux agent page type @5 TEXT"),
        "{}",
        paste.remedy()
    );
    let typed = require_terminal(Verb::Paste, &at(ResourceKind::Document)).unwrap_err();
    assert!(
        typed.remedy().contains("takes no input"),
        "{}",
        typed.remedy()
    );
    let prompt = require_terminal(Verb::AgentPrompt, &at(ResourceKind::AccessibilityTree));
    assert!(prompt.unwrap_err().remedy().contains("phux agent page"));
    let run = require_terminal(Verb::Run, &at(ResourceKind::Document)).unwrap_err();
    assert!(
        run.remedy().starts_with("address a Terminal"),
        "{}",
        run.remedy()
    );
}

#[test]
fn a_refusal_names_the_verb_the_resource_and_its_kind_and_what_to_do() {
    let keys = route(Verb::SendKeys, &at(ResourceKind::AccessibilityTree)).unwrap_err();
    assert_eq!(
        keys.to_string(),
        "`send-keys` does not apply to @5, an accessibility tree"
    );
    assert!(
        keys.remedy().contains("phux agent page key @5 KEY"),
        "{}",
        keys.remedy()
    );
    let typed = route(Verb::SendKeys, &at(ResourceKind::Document)).unwrap_err();
    assert!(!typed.remedy().contains("page"), "{}", typed.remedy());

    let surface = route(Verb::Wait, &at(ResourceKind::PixelSurface)).unwrap_err();
    assert!(
        surface.remedy().contains("--out FILE"),
        "{}",
        surface.remedy()
    );

    let session = route(Verb::Snapshot, &at(ResourceKind::AgentSession)).unwrap_err();
    assert!(session.to_string().ends_with("an agent session"));
    assert!(session.remedy().contains("phux agent log"));
    assert!(session.remedy().contains("its parent pane has the screen"));

    // A known parent is named, and the wording follows the verb.
    let bound = Resolved {
        parent: Some(ResourceId::local(3)),
        ..at(ResourceKind::AgentSession)
    };
    let remedy = |verb| require_terminal(verb, &bound).unwrap_err().remedy();
    assert!(remedy(Verb::Snapshot).contains("its parent pane @3 has the screen"));
    assert!(remedy(Verb::Paste).starts_with("type into its parent pane @3"));
    assert!(remedy(Verb::Run).starts_with("type into its parent pane @3"));
    for verb in [Verb::Resize, Verb::Signal, Verb::Upload] {
        let remedy = remedy(verb);
        assert_eq!(
            remedy,
            "address its parent pane @3, the terminal the agent runs in"
        );
    }

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

#[test]
fn control_characters_print_as_signs_and_cannot_act() {
    // OSC 52 (clipboard), a title, a forged OSC 7501, a C1 CSI, DEL.
    let hostile = "ok\x1b]52;c;aGk=\x07 \x1b]0;t\x1b\\ \x1b]7501;x\x07\u{9b}2J\x7f";
    let safe = terminal_safe(hostile);
    assert!(!safe.chars().any(char::is_control), "{safe:?}");
    assert_eq!(safe, "ok␛]52;c;aGk=␇ ␛]0;t␛\\ ␛]7501;x␇\u{fffd}2J␡");
    assert_eq!(terminal_safe("a\r\nb\rc\td\n"), "a\nb␍c\td\n");
    assert!(matches!(terminal_safe("plain\ttext\n"), Cow::Borrowed(_)));
}

#[test]
fn terminal_text_is_safe_and_ends_in_a_newline() {
    let markdown = doc(Some("text/markdown"), b"# done\x1b[2J");
    assert_eq!(
        terminal_text(Kind::Document, &markdown).as_deref(),
        Some("# done␛[2J\n")
    );
    let ended = doc(None, b"{}\n");
    assert_eq!(terminal_text(Kind::Tree, &ended).as_deref(), Some("{}\n"));
    assert_eq!(
        terminal_text(Kind::Tree, &doc(None, b"")).as_deref(),
        Some("")
    );
    assert_eq!(terminal_text(Kind::Surface, &ended), None);
}
