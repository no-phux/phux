use super::*;

#[test]
fn support_sequences_match_the_prototypes() {
    let mut rng = Rng::new(1);
    for expected in [0.62707394, 0.0027357212, 0.52744704, 0.98105097] {
        assert_eq!(rng.next(), expected);
    }
    assert_eq!(hash(""), 2_166_136_261);
    assert_eq!(hash("a"), 0xe40c292c);
    assert_eq!(hash("mini/🐠"), 0xfe56dc82, "UTF-16, not UTF-8 bytes");
}

#[test]
fn harnesses_share_colours_without_sharing_scene_geometry() {
    assert_eq!(species(Some("claude-code"), "Gemini"), &CLAUDE);
    assert_eq!(species(Some("codex"), "x"), &CODEX);
    assert_eq!(species(Some("opencode"), "x"), &OPENCODE);
    assert_eq!(species(Some("gemini-cli"), "x"), &GEMINI);
    assert_eq!(species(None, "Gemini"), &GEMINI);
    assert_eq!(species(Some("unknown"), "Claude"), &CLAUDE);
    assert_eq!(species(None, "build"), &OTHER);
    for style in [&CLAUDE, &CODEX, &OPENCODE, &GEMINI, &OTHER] {
        let reef = crate::reef::species(
            None,
            match style.harness {
                Harness::Claude => "claude",
                Harness::Codex => "codex",
                Harness::OpenCode => "opencode",
                Harness::Gemini => "gemini",
                Harness::Other => "other",
            },
        );
        assert_eq!(
            (reef.body, reef.belly, reef.fin),
            (style.body, style.belly, style.fin)
        );
        assert_eq!(reef.form, crate::creature::Form::of(style.harness));
    }
}

#[test]
fn marks_have_the_same_mood_in_every_scene() {
    for (mark, mood) in [
        (Mark::Blocked, Mood::Waiting),
        (Mark::Failed, Mood::Waiting),
        (Mark::Bell, Mood::Waiting),
        (Mark::Done, Mood::Done),
        (Mark::Succeeded, Mood::Done),
        (Mark::Working, Mood::Working),
        (Mark::Running, Mood::Working),
        (Mark::Background, Mood::Working),
        (Mark::Idle, Mood::Idle),
        (Mark::None, Mood::Idle),
    ] {
        assert_eq!(Mood::of(mark), mood);
    }
}
