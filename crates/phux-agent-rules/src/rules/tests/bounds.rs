    // --- Load-time bounds: an over-bound manifest is rejected whole --------

    fn manifest_with_rules(count: usize) -> String {
        let body = (0..count)
            .map(|idx| {
                format!(
                    "[[rules]]\nid = \"r{idx}\"\nstate = \"idle\"\nregion = \"title\"\n\
                     match = {{ contains = \"x{idx}\" }}"
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("kind = \"big\"\nbinaries = [\"big\"]\n{body}\n")
    }

    fn contains_rule(needle: &str) -> String {
        one_rule(&format!(
            "state = \"idle\"\nregion = \"title\"\nmatch = {{ contains = \"{needle}\" }}"
        ))
    }

    #[test]
    fn rule_count_and_pattern_length_are_capped_inclusively() {
        let set = install(&manifest_with_rules(super::MAX_RULES_PER_MANIFEST)).expect("at cap");
        assert_eq!(set.manifest("big").expect("manifest").rules.len(), 128);

        let spec: ManifestSpec =
            toml::from_str(&manifest_with_rules(super::MAX_RULES_PER_MANIFEST + 1))
                .expect("parses");
        let mut set = RuleSet::default();
        assert!(
            set.install(spec)
                .expect_err("over cap")
                .contains("over the")
        );
        assert!(
            set.is_empty() && set.kind_for_binary("big").is_none(),
            "no partial state"
        );

        let cap = super::MAX_MATCHER_CHARS;
        assert!(install(&contains_rule(&"a".repeat(cap))).is_ok());
        let err = install(&contains_rule(&"a".repeat(cap + 1))).expect_err("too long");
        assert!(err.contains("over the"), "{err}");
        // Counted in characters, so multi-byte agent chrome is not penalized.
        assert!(install(&contains_rule(&"\u{2500}".repeat(cap))).is_ok());
    }

    /// The schema bounds nesting itself (depth 8 loads, 9 does not), and a
    /// pathological document is refused by the TOML parser, never
    /// overflowing the stack.
    #[test]
    fn deep_nesting_is_bounded_and_never_overflows_the_stack() {
        let nested = |depth: usize| {
            one_rule(&format!(
                "state = \"idle\"\nregion = \"title\"\nmatch = {}{{ contains = \"x\" }}{}",
                "{ not = ".repeat(depth),
                " }".repeat(depth),
            ))
        };
        assert!(install(&nested(super::MAX_PREDICATE_DEPTH - 1)).is_ok());
        let err = install(&nested(super::MAX_PREDICATE_DEPTH)).expect_err("too deep");
        assert!(err.contains("nests deeper than"), "{err}");
        assert!(toml::from_str::<ManifestSpec>(&nested(20_000)).is_err());
    }

    /// The matcher budget is per rule and also cumulative across the
    /// manifest (otherwise the manifest cap would be unreachable).
    #[test]
    fn the_matcher_budget_is_per_rule_and_cumulative() {
        let rule = |id: usize, matchers: usize| {
            let children: Vec<String> = (0..matchers)
                .map(|idx| format!("{{ contains = \"x{idx}\" }}"))
                .collect();
            format!(
                "[[rules]]\nid = \"r{id}\"\nstate = \"idle\"\nregion = \"title\"\n\
                 match = {{ any = [{}] }}\n",
                children.join(", ")
            )
        };
        let manifest = |rules: usize, matchers: usize| {
            (0..rules).fold(
                String::from("kind = \"k\"\nbinaries = [\"k\"]\n"),
                |mut m, id| {
                    m.push_str(&rule(id, matchers));
                    m
                },
            )
        };
        let per_rule = super::MAX_MATCHERS_PER_RULE;
        assert!(install(&manifest(2, per_rule)).is_ok());
        let err = install(&manifest(1, per_rule + 1)).expect_err("per rule");
        assert!(err.contains("matchers in one rule"), "{err}");
        let rules_needed = super::MAX_MATCHERS_PER_MANIFEST / per_rule + 1;
        let err = install(&manifest(rules_needed, per_rule)).expect_err("aggregate");
        assert!(err.contains("matchers in the manifest"), "{err}");
    }

    /// Every built-in compiles well inside the bounds and owns each declared
    /// binary alias; otherwise that agent silently disappears in production.
    #[test]
    fn every_builtin_manifest_compiles_and_indexes_its_binaries() {
        let expected = [
            ("claude", &["claude", "claude-code"][..]),
            ("fx", &["fx"][..]),
            ("codex", &["codex"][..]),
            ("opencode", &["opencode", "opencode2", "@opencode-ai"][..]),
            ("pi", &["pi"][..]),
            ("omp", &["omp"][..]),
            ("grok", &["grok"][..]),
            ("amp", &["amp"][..]),
            ("cursor-agent", &["cursor-agent"][..]),
            ("gemini", &["gemini"][..]),
            ("goose", &["goose"][..]),
            ("aider", &["aider"][..]),
        ];
        for (kind, binaries) in expected {
            let spec: ManifestSpec = toml::from_str(builtin(kind)).expect("builtin parses");
            assert!(
                spec.rules.len() <= super::MAX_RULES_PER_MANIFEST / 4,
                "{kind}"
            );
            let set = compile(builtin(kind));
            for binary in binaries {
                assert_eq!(set.kind_for_binary(binary), Some(kind));
            }
            let manifest = set.manifest(kind).expect("manifest");
            assert_eq!(manifest.name, kind);
            // fx identity is known, but its TUI has no verified status fixture.
            if kind != "fx" {
                assert!(!manifest.rules.is_empty());
            }
        }
        let set = global();
        assert!(std::rc::Rc::ptr_eq(&set, &global()), "memoized");
        assert_eq!(set.kind_for_binary("codex"), Some("codex"));
        assert_eq!(set.kind_for_binary("opencode2"), Some("opencode"));
    }
