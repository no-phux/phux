//! Onboarding's state: steps, resuming, and what the CLI said.

use rune::prefs::Prefs;

use super::*;
use crate::launch::Resolved;

fn desk(prefs: Prefs) -> Desk {
    let mut desk = Desk::new(&Resolved::offline("onboarding"));
    desk.prefs = prefs;
    desk.launch.aimed = false;
    desk
}

fn skills(detected: &[&str]) -> Probe<Vec<AgentSkill>> {
    Probe::Ready(
        detected
            .iter()
            .map(|agent| AgentSkill {
                agent: (*agent).to_owned(),
                detected: true,
                ..AgentSkill::default()
            })
            .collect(),
    )
}

#[test]
fn quitting_mid_way_resumes_at_the_step() {
    let dir = std::env::temp_dir().join(format!("phux-rune-onb-{}", std::process::id()));
    let file = dir.join("prefs.json");
    let mut first = desk(Prefs::at(&file));
    first.first_run();
    assert!(first.onboarding.open);
    assert_eq!(first.onboarding.step, Step::Welcome);
    first.onboarding_continue();
    first.onboarding_continue();
    assert_eq!(first.onboarding.step, Step::Agents);
    drop(first);

    let mut again = desk(Prefs::at(&file));
    again.first_run();
    assert!(again.onboarding.open);
    assert_eq!(
        again.onboarding.step,
        Step::Agents,
        "picks up where it was left"
    );
    again.skip_onboarding();
    drop(again);

    let mut after = desk(Prefs::at(&file));
    after.first_run();
    assert!(!after.onboarding.open, "never opens on its own once done");
    assert!(after.notice.is_some(), "the tour's tip instead");
    after.open_onboarding();
    assert_eq!(
        after.onboarding.step,
        Step::Welcome,
        "reopening starts over"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_development_run_never_opens_it_on_its_own() {
    let mut desk = desk(Prefs::memory());
    desk.launch.aimed = true;
    desk.first_run();
    assert!(!desk.onboarding.open);
}

#[test]
fn bringing_your_setup_shows_only_with_an_agent() {
    let mut desk = desk(Prefs::memory());
    desk.open_onboarding();
    desk.go_to(Step::Agents);
    desk.onboarding.skills = skills(&["pi"]);
    desk.onboarding_continue();
    assert_eq!(desk.onboarding.step, Step::Done, "no agent to do it");
    desk.onboarding_back();
    assert_eq!(desk.onboarding.step, Step::Agents);
    desk.onboarding.skills = skills(&["codex"]);
    desk.onboarding_continue();
    assert_eq!(desk.onboarding.step, Step::Migrate);
    assert_eq!(desk.onboarding.migrators(), ["codex"]);
}

#[test]
fn keys_continue_step_back_and_skip() {
    let key = |key| KeyEvent {
        key,
        mods: rune::Mods::default(),
    };
    let mut desk = desk(Prefs::memory());
    desk.open_onboarding();
    assert!(desk.onboarding_key(&key(Key::Enter)));
    assert_eq!(desk.onboarding.step, Step::Look);
    desk.open_grant_sheet();
    assert!(desk.onboarding_key(&key(Key::Escape)));
    assert!(
        !desk.onboarding.grant_sheet,
        "Escape closes the sheet first"
    );
    assert_eq!(desk.onboarding.step, Step::Look);
    assert!(desk.onboarding_key(&key(Key::Escape)));
    assert_eq!(desk.onboarding.step, Step::Welcome);
    assert!(desk.onboarding_key(&key(Key::Escape)));
    assert!(!desk.onboarding.open, "Escape on the first step skips");
}

#[test]
fn a_missing_command_reads_as_update_phux() {
    let probe: Probe<()> = Probe::from(Err("error: unrecognized subcommand 'skill'".into()));
    assert_eq!(probe, Probe::Unsupported);
    let probe: Probe<()> = Probe::from(Err("timed out".into()));
    assert_eq!(probe, Probe::Failed("timed out".into()));
}
