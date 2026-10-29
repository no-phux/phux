    /// `(kind, title, progress, screen, want)`.
    type GoldenCase = (String, String, String, Vec<String>, String);

    /// Synthetic screens built from, or around, the captures.
    fn named_screen(name: &str) -> Vec<String> {
        match name {
            "claude-transcript-dialog" => [
                lines(&[
                    "  Here is what that prompt looks like:",
                    "  > Do you want to proceed?",
                    "  > \u{276f} 1. Yes",
                    "  > 2. No",
                ]),
                golden("claude/idle_prompt.txt"),
            ]
            .concat(),
            "bare-dialog" => lines(&["  Do you want to proceed?", "  \u{276f} 1. Yes", "  2. No"]),
            "claude-pager" => lines(&[
                "  Do you want to proceed?",
                "  1. Yes",
                "  Showing detailed transcript \u{00b7} ctrl+o to toggle \u{00b7} \u{2191}\u{2193} scroll",
            ]),
            "codex-prose" => lines(&[
                "  I was going to ask: would you like to run the following command?",
                "  ...but I decided against it.",
            ]),
            "grok-quoted-dialog" => {
                let mut screen = golden("grok/idle_prompt.txt");
                screen.splice(
                    4..4,
                    lines(&[
                        "Do you trust the contents of this directory?",
                        "Yes, proceed                 y",
                        "No, quit                     n",
                    ]),
                );
                screen
            }
            "goose-dialog-above-idle" => [
                golden("goose/blocked_tool_approval.txt"),
                golden("goose/idle_prompt.txt"),
            ]
            .concat(),
            "aider-dialog-above-idle" => [
                golden("aider/blocked_shell_confirm.txt"),
                golden("aider/idle_prompt.txt"),
            ]
            .concat(),
            "amp-idle" => lines(&["╰──────────────── /tmp/ws ─╯"]),
            "amp-working" => lines(&["╰ ≈ Waiting ───── /tmp/ws ─╯"]),
            "cursor-login" => lines(&["Press any key to log in", "Signing in with the browser"]),
            // Both dialogs' text quoted in the transcript above a live footer.
            "gemini-quoted-dialogs" => [
                golden("gemini/blocked_shell_approval.txt"),
                golden("gemini/blocked_trust.txt"),
                golden("gemini/idle_prompt.txt"),
            ]
            .concat(),
            "pi-dialog-above-idle" => {
                [golden("pi/blocked_trust.txt"), golden("pi/idle_prompt.txt")].concat()
            }
            "omp-dialog-above-idle" => [
                golden("omp/blocked_tool_approval.txt"),
                golden("omp/idle_prompt.txt"),
            ]
            .concat(),
            "omp18-dialog-above-idle" => [
                golden("omp/v18_blocked_tool_approval.txt"),
                golden("omp/v18_idle_prompt.txt"),
            ]
            .concat(),
            other => panic!("unknown screen @{other}"),
        }
    }

    /// One golden case per title, all against the same `fixture` screen.
    fn sweep(kind: &str, fixture: &str, titles: Vec<String>, want: &str) -> Vec<GoldenCase> {
        titles
            .into_iter()
            .map(|title| {
                (
                    kind.to_owned(),
                    title,
                    String::new(),
                    golden(fixture),
                    want.to_owned(),
                )
            })
            .collect()
    }

    /// Gemini CLI 0.63.0 `computeTerminalTitle`: the captured `\u{2726}`
    /// Working title plus the source-stated, uncaptured `\u{23f2}` silent
    /// work and thought-subject variants; the Ready title asserts nothing.
    fn gemini_title_sweeps() -> Vec<GoldenCase> {
        let mut cases = Vec::new();
        cases.extend(sweep(
            "gemini",
            "gemini/idle_prompt.txt",
            [
                "\u{2726}  Working\u{2026} (work)",
                "\u{2726}  Reading the build script (work)",
                "\u{23f2}  Working\u{2026} (work)",
            ]
            .map(str::to_owned)
            .to_vec(),
            "working title-working",
        ));
        cases.extend(sweep(
            "gemini",
            "gemini/idle_prompt.txt",
            [
                "\u{25c7}  Ready (work)",
                "Gemini CLI (work)",
                "\u{2726} not the two-space separator",
            ]
            .map(str::to_owned)
            .to_vec(),
            "nothing",
        ));
        cases
    }

    /// Every golden case as `(kind, title, progress, screen, want)`, plus
    /// the title sweeps: every Claude busy frame, the whole Codex braille
    /// block, and the captured Grok title lists.
    fn golden_cases() -> Vec<GoldenCase> {
        let mut cases: Vec<_> = GOLDEN
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let fields: Vec<&str> = line.split(" | ").map(str::trim).collect();
                let [kind, title @ .., progress, screen, want] = fields.as_slice() else {
                    panic!("bad golden row: {line}");
                };
                let screen = screen
                    .strip_prefix('@')
                    .map_or_else(|| golden(screen), named_screen);
                (
                    (*kind).to_owned(),
                    title.join(" | "),
                    (*progress).to_owned(),
                    screen,
                    (*want).to_owned(),
                )
            })
            .collect();
        let busy = "working title-busy-spinner";
        cases.extend(sweep(
            "claude",
            "claude/idle_prompt.txt",
            CLAUDE_BUSY.map(str::to_owned).to_vec(),
            busy,
        ));
        cases.extend(sweep(
            "codex",
            "codex/idle_prompt.txt",
            [
                '\u{2800}', '\u{280b}', '\u{280f}', '\u{2834}', '\u{283c}', '\u{28ff}',
            ]
            .map(|cp| format!("{cp} tmp"))
            .to_vec(),
            busy,
        ));
        // OMP's title contract (17.1.2 and 18.6.1 source): `π <sep> <label>`,
        // with every `tui.titleSpinner` frame and the static `:` as working.
        cases.extend(sweep(
            "omp",
            "omp/v18_idle_prompt.txt",
            OMP_WORKING_SEPARATORS
                .iter()
                .flat_map(|sep| [format!("\u{3c0} {sep} label"), format!("\u{3c0} {sep}")])
                .collect(),
            "working title-working-spinner",
        ));
        cases.extend(sweep(
            "omp",
            "omp/v18_idle_prompt.txt",
            [
                "\u{3c0} > label",
                "\u{3c0} >",
                "\u{3c0}: label",
                "\u{3c0}",
                "custom title",
            ]
            .map(str::to_owned)
            .to_vec(),
            "nothing",
        ));
        cases.extend(sweep(
            "omp",
            "omp/v18_idle_prompt.txt",
            ["\u{3c0} ! label", "\u{3c0} !"].map(str::to_owned).to_vec(),
            "blocked title-attention",
        ));
        cases.extend(gemini_title_sweeps());
        let grok_titles = |text: &str| text.lines().map(str::to_owned).collect();
        cases.extend(sweep(
            "grok",
            "grok/idle_prompt.txt",
            grok_titles(include_str!("../../fixtures/grok/titles_working.txt")),
            busy,
        ));
        cases.extend(sweep(
            "grok",
            "grok/idle_prompt.txt",
            grok_titles(include_str!("../../fixtures/grok/titles_idle.txt")),
            "nothing",
        ));
        cases
    }

    fn state_word(state: Option<DetectedState>) -> &'static str {
        state.map_or("", DetectedState::as_str)
    }

    /// Each shipped manifest reads its captured screens as exactly their
    /// live state.
    #[test]
    fn shipped_manifests_read_captured_screens_correctly() {
        let mut sets = std::collections::HashMap::new();
        for (kind, title, progress, screen, want) in golden_cases() {
            let set = sets
                .entry(kind.clone())
                .or_insert_with(|| compile(builtin(&kind)));
            let got = eval(set, &kind, &title, &progress, &screen);
            let ok = match want.as_str() {
                "nothing" => got.state.is_none() && !got.freeze,
                "not-blocked" => got.state != Some(Blocked),
                "freeze" => got.freeze,
                expected => {
                    let actual = format!(
                        "{} {}",
                        state_word(got.state),
                        got.matched.as_deref().unwrap_or("")
                    );
                    actual == expected
                }
            };
            assert!(
                ok,
                "{kind} title={title:?} progress={progress:?}: want {want}, got {got:?}"
            );
        }
    }

    /// Claude's busy-title rule answers to REAL captured OSC 0 bytes, not
    /// this module's constants: the 2.1.228 glyph change went unnoticed
    /// while tests and rule agreed with each other. The capture's titles must
    /// partition into a quiet one (asserts nothing) and busy frames
    /// (`working`).
    #[test]
    fn every_busy_title_in_the_committed_capture_reads_as_working() {
        let capture = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR")
                .expect("the test runner sets CARGO_MANIFEST_DIR"),
        )
        .join("src/fixtures/claude/osc-9-4-capture/claude-title-enabled.rawcap");
        let raw = std::fs::read_to_string(&capture).unwrap_or_else(|e| {
            panic!(
                "{}: {e}. If this capture is removed, re-verify `title-busy-spinner` against a \
                 fresh one rather than deleting this test.",
                capture.display(),
            )
        });
        let mut titles: Vec<&str> = raw
            .split("\u{1b}]0;")
            .skip(1)
            .filter_map(|rest| rest.split('\u{7}').next())
            .filter(|t| !t.is_empty())
            .collect();
        titles.sort_unstable();
        titles.dedup();

        let set = compile(builtin("claude"));
        let idle = golden("claude/idle_prompt.txt");
        let (mut busy, mut quiet) = (0, 0);
        for title in titles {
            let got = eval(&set, "claude", title, "", &idle);
            if title.starts_with('\u{2733}') {
                quiet += 1;
                assert_eq!(got.state, None, "{title:?}");
            } else {
                busy += 1;
                assert_eq!(
                    got.matched.as_deref(),
                    Some("title-busy-spinner"),
                    "{title:?}"
                );
            }
        }
        assert!(busy >= 2 && quiet >= 1, "busy={busy} quiet={quiet}");
    }

    /// The non-short-circuiting trace walker must agree with the production
    /// matcher on every rule of every golden, and must evaluate every child
    /// of a combinator so an author can see which conjunct failed.
    #[test]
    fn the_trace_agrees_with_the_production_evaluator() {
        let mut sets = std::collections::HashMap::new();
        for (kind, _, _, screen, _) in golden_cases() {
            let set = sets
                .entry(kind.clone())
                .or_insert_with(|| compile(builtin(&kind)));
            let manifest = set.manifest(&kind).expect("manifest");
            for title in ["", CLAUDE_BUSY[0], CLAUDE_QUIET, "\u{280b} tmp"] {
                let screen = Screen {
                    title,
                    progress: "",
                    lines: &screen,
                };
                let explained = manifest.explain(&screen);
                assert_eq!(manifest.evaluate(&screen), explained.evaluation, "{kind}");
                assert_eq!(explained.rules.len(), manifest.rules.len(), "{kind}");
                for trace in &explained.rules {
                    assert_eq!(trace.matched, trace.evidence.matched, "{kind}/{}", trace.id);
                }
            }
        }

        let set = compile(SAMPLE);
        let buf = lines(&["nothing here at all"]);
        let explained = set.manifest("sample").expect("manifest").explain(&Screen {
            title: "",
            progress: "",
            lines: &buf,
        });
        let rule = explained
            .rules
            .iter()
            .find(|r| r.id == "screen-blocked")
            .expect("rule reported");
        assert!(!rule.matched);
        assert_eq!(rule.evidence.op, "all");
        assert_eq!(rule.evidence.children.len(), 2);
        assert!(
            rule.evidence
                .children
                .iter()
                .all(|c| !c.matched && c.pattern.is_some())
        );
    }
