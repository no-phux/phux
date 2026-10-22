//! `plugin-action` dispatch: the action id arrives as `id`, the spelling a
//! config binding can write, or as the older `action`.

use crate::attach::jobs::{Job, JobKind};
use crate::layout::ArrangementDoc;

use super::args::bare_action;
use super::test_support::{act, fx, tid};

fn starts_a_job(action: &phux_config::keybind::ResolvedAction) -> bool {
    let effects = fx(ArrangementDoc::single(tid(1))).run(action);
    let started = effects.job.as_ref().map(Job::kind) == Some(JobKind::PluginAction);
    assert_eq!(started, !effects.bell, "a run that starts nothing bells");
    started
}

#[test]
fn plugin_action_runs_from_id_or_the_older_action_arg() {
    for id_arg in ["id", "action"] {
        let action = act(
            "plugin-action",
            &[
                ("plugin", "com.example.tools".into()),
                (id_arg, "summarize".into()),
            ],
        );
        assert!(starts_a_job(&action), "`{id_arg}` names the action");
    }
}

#[test]
fn plugin_action_bound_in_config_runs() {
    let cfg = phux_config::parse_with_defaults(
        "[keybindings.prefix-table]\n\
         S = { action = \"plugin-action\", plugin = \"com.example.tools\", id = \"summarize\" }\n",
        std::path::Path::new("config.toml"),
    )
    .expect("a plugin-action binding parses");
    let binding = cfg.keybindings.prefix_table.get("S").expect("bound");
    assert!(starts_a_job(&phux_config::keybind::ResolvedAction::from(
        binding
    )));
}

#[test]
fn plugin_action_without_its_args_bells() {
    assert!(!starts_a_job(&bare_action("plugin-action")));
    assert!(!starts_a_job(&act(
        "plugin-action",
        &[("plugin", "com.example.tools".into())]
    )));
}
