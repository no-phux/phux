    use super::{Evaluation, ManifestSpec, RuleSet, global};
    use crate::DetectedState::{self, Blocked, Idle, Working};
    use crate::regions::Screen;

    fn compile(toml_text: &str) -> RuleSet {
        let spec: ManifestSpec = toml::from_str(toml_text).expect("manifest parses");
        let mut set = RuleSet::default();
        set.install(spec).expect("manifest compiles");
        set
    }

    fn builtin(kind: &str) -> &'static str {
        super::BUILTIN_MANIFESTS
            .iter()
            .find_map(|(candidate, manifest)| (*candidate == kind).then_some(*manifest))
            .expect("built-in manifest")
    }

    fn lines(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| (*s).to_owned()).collect()
    }

    /// A committed golden viewport under `src/fixtures/`.
    fn golden(rel: &str) -> Vec<String> {
        let path = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR")
                .expect("the test runner sets CARGO_MANIFEST_DIR"),
        )
        .join("src/fixtures")
        .join(rel);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn eval(set: &RuleSet, kind: &str, title: &str, progress: &str, buf: &[String]) -> Evaluation {
        set.manifest(kind).expect("manifest").evaluate(&Screen {
            title,
            progress,
            lines: buf,
        })
    }

    fn install(toml_text: &str) -> Result<RuleSet, String> {
        let spec: ManifestSpec = toml::from_str(toml_text).map_err(|e| e.to_string())?;
        let mut set = RuleSet::default();
        set.install(spec)?;
        Ok(set)
    }

    /// A one-rule manifest around `rule_body` (the lines after `[[rules]]`).
    fn one_rule(rule_body: &str) -> String {
        format!("kind = \"k\"\nbinaries = [\"k\"]\n[[rules]]\nid = \"r\"\n{rule_body}\n")
    }

    const SAMPLE: &str = r#"
kind = "sample"
name = "Sample"
binaries = ["sample", "sample-cli"]

[[rules]]
id = "title-working"
state = "working"
priority = 10
region = "title"
match = { line-regex = "^W " }

[[rules]]
id = "screen-blocked"
state = "blocked"
priority = 90
region = "bottom-lines"
match = { all = [ { contains = "do you want" }, { line-regex = "^\\s*\\d+\\." } ] }

[[rules]]
id = "screen-idle"
state = "idle"
priority = 40
region = "bottom-lines"
visible-idle = true
match = { contains = "ready" }

[[rules]]
id = "pager"
priority = 200
region = "bottom-lines"
skip-state-update = true
match = { contains = "-- pager --" }
"#;

    /// `(title, screen, state, matched rule, visible_idle, freeze)`.
    type SampleCase<'a> = (
        &'a str,
        &'a [&'a str],
        Option<DetectedState>,
        Option<&'a str>,
        bool,
        bool,
    );

    /// Title rules outrank screen rules regardless of priority, priority
    /// orders within a class, `all` needs every child, flags union across
    /// matching rules, and nothing matching yields no state (the caller's
    /// fail-safe decides).
    #[test]
    fn evaluation_orders_rules_and_unions_flags() {
        let set = compile(SAMPLE);
        assert_eq!(set.kind_for_binary("SAMPLE-CLI"), Some("sample"));
        assert_eq!(set.kind_for_binary("nope"), None);
        let dialog = ["do you want to proceed?", " 1. Yes"];
        let cases: &[SampleCase<'_>] = &[
            (
                "W busy",
                &dialog,
                Some(Working),
                Some("title-working"),
                false,
                false,
            ),
            (
                "idle",
                &["ready", dialog[0], dialog[1]],
                Some(Blocked),
                Some("screen-blocked"),
                true,
                false,
            ),
            ("", &[dialog[0]], None, None, false, false),
            ("", &["nothing interesting here"], None, None, false, false),
            (
                "",
                &[dialog[0], dialog[1], "-- pager --"],
                Some(Blocked),
                Some("screen-blocked"),
                false,
                true,
            ),
        ];
        for &(title, screen, state, matched, visible_idle, freeze) in cases {
            let got = eval(&set, "sample", title, "", &lines(screen));
            assert_eq!(got.state, state, "{title} {screen:?}");
            assert_eq!(got.matched.as_deref(), matched, "{title} {screen:?}");
            assert_eq!(
                (got.visible_idle, got.freeze),
                (visible_idle, freeze),
                "{screen:?}"
            );
        }
    }

    #[test]
    fn not_combinator_negates() {
        let set = compile(&one_rule(
            "state = \"idle\"\nregion = \"viewport\"\n\
             match = { all = [ { contains = \"prompt\" }, { not = { contains = \"pager\" } } ] }",
        ));
        assert_eq!(
            eval(&set, "k", "", "", &lines(&["prompt", "pager"])).state,
            None
        );
        assert_eq!(
            eval(&set, "k", "", "", &lines(&["prompt"])).state,
            Some(Idle)
        );
    }

    /// Kebab-case flags bind (without `rename_all` they would be silently
    /// ignored), and any unusable manifest is rejected whole: a bad regex,
    /// an unknown state, a removed or mis-cased flag, a malformed region.
    #[test]
    fn manifests_bind_their_flags_or_are_rejected_whole() {
        let set = compile(&one_rule(
            "state = \"idle\"\nregion = \"title\"\nvisible-idle = true\n\
             skip-state-update = true\nmatch = { contains = \"x\" }",
        ));
        let rule = &set.manifest("k").expect("manifest").rules[0];
        assert!(rule.visible_idle && rule.skip_state_update);

        for body in [
            "state = \"idle\"\nregion = \"title\"\nmatch = { regex = \"(unclosed\" }",
            "state = \"confused\"\nregion = \"title\"\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"title\"\nvisible-blocker = true\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"title\"\nvisible-working = true\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"title\"\nvisible_idle = true\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"bottom-lines(0)\"\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"title(2)\"\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"bottom_lines\"\nmatch = { contains = \"x\" }",
            "state = \"idle\"\nregion = \"nonsense\"\nmatch = { contains = \"x\" }",
        ] {
            assert!(install(&one_rule(body)).is_err(), "{body}");
        }
    }

    /// The shipped positive-idle sources are Claude's OSC 9;4 remove signal
    /// and Gemini CLI's state-less `Ready` title, each from a single-payload
    /// region; idle is otherwise the fail-safe reached by nothing matching.
    #[test]
    fn only_single_payload_captured_signals_set_visible_idle() {
        let visible_idle: Vec<(&str, String)> = super::BUILTIN_MANIFESTS
            .iter()
            .flat_map(|(kind, text)| {
                let spec: ManifestSpec = toml::from_str(text).expect("builtin parses");
                spec.rules
                    .into_iter()
                    .filter(|rule| rule.visible_idle)
                    .map(move |rule| (*kind, rule.id))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(
            visible_idle,
            [
                ("claude", "osc-progress-idle".to_owned()),
                ("gemini", "title-ready".to_owned()),
            ]
        );
    }

    /// Gemini's `Ready` title arms the idle bypass without asserting a state,
    /// so the folder-trust dialog painted under that same title still wins.
    #[test]
    fn gemini_ready_title_is_visible_idle_but_never_masks_a_dialog() {
        let set = compile(builtin("gemini"));
        let ready = "\u{25c7}  Ready (work)";
        let idle = eval(&set, "gemini", ready, "", &golden("gemini/idle_prompt.txt"));
        assert_eq!((idle.state, idle.visible_idle), (None, true));
        let trust = eval(
            &set,
            "gemini",
            ready,
            "",
            &golden("gemini/blocked_trust.txt"),
        );
        assert_eq!(trust.state, Some(Blocked));
        let busy = "\u{2726}  Working\u{2026} (work)";
        let working = eval(&set, "gemini", busy, "", &golden("gemini/working.txt"));
        assert_eq!(
            (working.state, working.visible_idle),
            (Some(Working), false)
        );
    }

    /// Windowed regions are distinct: two rules with different N read
    /// different text, and a top-anchored window reads only the banner.
    #[test]
    fn windowed_regions_read_their_own_text() {
        let set = compile(
            r#"
kind = "w"
binaries = ["w"]
[[rules]]
id = "last-row-only"
state = "working"
priority = 10
region = "bottom-lines(1)"
match = { contains = "spinner" }
[[rules]]
id = "footer-block"
state = "blocked"
priority = 20
region = "bottom-lines(6)"
match = { contains = "spinner" }
[[rules]]
id = "banner"
state = "idle"
priority = 30
region = "top-non-empty-lines"
match = { contains = "thinking" }
"#,
        );
        let got = eval(
            &set,
            "w",
            "",
            "",
            &lines(&["spinner", "a", "b", "c", "d", "e"]),
        );
        assert_eq!(got.matched.as_deref(), Some("footer-block"));
        let got = eval(&set, "w", "", "", &lines(&["a", "spinner"]));
        assert_eq!(got.matched.as_deref(), Some("footer-block"), "20 beats 10");
        let got = eval(&set, "w", "", "", &lines(&["", "  thinking...", "x"]));
        assert_eq!(got.matched.as_deref(), Some("banner"));
        let got = eval(&set, "w", "", "", &lines(&["  header", "  thinking..."]));
        assert_eq!(got.state, None, "a bare top window is one row");
    }

    /// The explainer previews the default regions plus every window the
    /// manifest names, once each, in the spelling an operator types.
    #[test]
    fn the_explainer_previews_every_window_the_manifest_names() {
        const DEFAULTS: [&str; 6] = [
            "title",
            "osc-progress",
            "prompt-box",
            "after-last-rule",
            "bottom-lines",
            "viewport",
        ];
        let set = compile(
            "kind = \"w\"\nbinaries = [\"w\"]\n\
             [[rules]]\nid = \"wide\"\nstate = \"idle\"\nregion = \"bottom-lines(14)\"\n\
             match = { contains = \"zzz\" }\n\
             [[rules]]\nid = \"banner\"\nstate = \"idle\"\nregion = \"top-non-empty-lines(2)\"\n\
             match = { contains = \"zzz\" }\n",
        );
        let buf = lines(&["a", "b", "c"]);
        let screen = Screen {
            title: "",
            progress: "",
            lines: &buf,
        };
        let explained = set.manifest("w").expect("manifest").explain(&screen);
        let names: Vec<String> = explained.regions.iter().map(|(r, _)| r.as_str()).collect();
        let windows = ["bottom-lines(14)", "top-non-empty-lines(2)"];
        assert_eq!(names, [&DEFAULTS[..], &windows[..]].concat());
        let rules: Vec<&str> = explained.rules.iter().map(|r| r.region.as_str()).collect();
        assert_eq!(rules, windows);

        // A bare `bottom-lines` is the default window, already listed.
        let set = compile(SAMPLE);
        let explained = set.manifest("sample").expect("manifest").explain(&screen);
        let names: Vec<String> = explained.regions.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(names, DEFAULTS);
    }

    include!("tests/bounds.rs");
    include!("tests/fixtures.rs");
    include!("tests/golden.rs");
