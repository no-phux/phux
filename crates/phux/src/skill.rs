//! Compiled agent-skill rendering.

use std::process::ExitCode;

use usage::ValueEnum;

/// Amount and subject of agent guidance to print.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub(crate) enum SkillScope {
    /// Essential read-act-wait-verify guidance and safety rules.
    Quick,
    /// Quick guidance plus agent identity and lifecycle supervision.
    Agent,
    /// Quick guidance plus terminal screen and input mechanics.
    Terminal,
    /// All workflow guidance and discovery commands.
    #[default]
    Full,
}

pub(crate) const SOURCE: &str = include_str!("../../../.agents/skills/using-phux/SKILL.md");
const MARKER_PREFIX: &str = "<!-- phux-skill-region: ";

impl SkillScope {
    fn includes(self, region: Self) -> bool {
        self == Self::Full || self == region || region == Self::Quick
    }
}

fn marker(line: &str) -> Option<SkillScope> {
    let name = line
        .trim()
        .strip_prefix(MARKER_PREFIX)?
        .strip_suffix(" -->")?;
    match name {
        "quick" => Some(SkillScope::Quick),
        "agent" => Some(SkillScope::Agent),
        "terminal" => Some(SkillScope::Terminal),
        "full" => Some(SkillScope::Full),
        _ => None,
    }
}

/// Render one scope from the single marked source document.
pub(crate) fn render(scope: SkillScope) -> String {
    let mut region = SkillScope::Quick;
    let mut rendered = String::with_capacity(SOURCE.len());
    for line in SOURCE.split_inclusive('\n') {
        if let Some(next) = marker(line) {
            region = next;
        } else if scope.includes(region) {
            rendered.push_str(line);
        }
    }
    rendered
}

pub(crate) fn run(scope: SkillScope) -> ExitCode {
    let rendered = render(scope);
    crate::output::bytes(rendered.as_bytes());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::{SOURCE, SkillScope, marker, render};

    #[test]
    fn every_marker_is_known_and_never_leaks() {
        let marker_lines: Vec<_> = SOURCE
            .lines()
            .filter(|line| line.contains("phux-skill-region:"))
            .collect();
        assert!(!marker_lines.is_empty());
        assert!(marker_lines.iter().all(|line| marker(line).is_some()));

        for scope in [
            SkillScope::Quick,
            SkillScope::Agent,
            SkillScope::Terminal,
            SkillScope::Full,
        ] {
            let output = render(scope);
            assert!(output.starts_with("---\nname: using-phux\n"));
            assert!(output.contains(concat!(
                "  version: \"",
                env!("CARGO_PKG_VERSION"),
                "\" # x-release-please-version\n"
            )));
            assert!(output.ends_with('\n'));
            assert!(!output.contains("phux-skill-region:"));
        }
    }

    #[test]
    fn scopes_are_distinct_and_carry_their_subjects() {
        let quick = render(SkillScope::Quick);
        let agent = render(SkillScope::Agent);
        let terminal = render(SkillScope::Terminal);
        let full = render(SkillScope::Full);

        assert!(quick.contains("## Workflow"));
        assert!(!quick.contains("## Supervising agents"));
        assert!(!quick.contains("--cells"));
        assert!(agent.contains("## Supervising agents"));
        assert!(!agent.contains("--cells"));
        assert!(terminal.contains("--cells"));
        assert!(!terminal.contains("## Supervising agents"));
        assert!(full.contains("Discovering the rest of the surface"));
        assert!(full.len() > agent.len());
        assert!(full.len() > terminal.len());
    }
}
