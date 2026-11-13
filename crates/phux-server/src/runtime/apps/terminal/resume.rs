//! OSC 7501 `resume=`: a program's resume token, kept on its pane's launch
//! record and spent on the user's `[[restore.program]]` command at relaunch.
//!
//! The token never becomes metadata and `org_wire` does not map it, so no
//! client and no hub sees it. The pane only supplies an id: what runs is
//! the argv the user wrote for that app.

use phux_core::{LaunchSpec, Op, ResumeToken, TerminalLaunch};
use phux_protocol::ids::ResourceId;
use tokio::sync::watch;

use crate::state::{ServerState, SharedState};

/// Commit each change the pane's engine reports to its launch record.
pub(in crate::runtime) fn spawn_drain(
    state: SharedState,
    terminal: ResourceId,
    mut rx: watch::Receiver<Option<ResumeToken>>,
) {
    tokio::task::spawn_local(async move {
        // Changes only: the channel starts empty, the record may not.
        while rx.changed().await.is_ok() {
            let token = rx.borrow_and_update().clone();
            state.with_mut(|s| report(s, &terminal, token));
        }
    });
}

/// Record `token` as what a relaunch of `terminal` resumes with; `None`
/// forgets it. Nothing is written when the record already says so.
fn report(s: &mut ServerState, terminal: &ResourceId, token: Option<ResumeToken>) {
    // A stopping server ends every program; that is not the user quitting.
    if s.shutting_down() {
        return;
    }
    let Some(record) = s
        .resource_from_wire(terminal)
        .and_then(|id| s.registry().record(id))
    else {
        return;
    };
    let (Some(activation), Some(LaunchSpec::Terminal(launch))) =
        (record.activation.as_ref(), record.launch.as_ref())
    else {
        return;
    };
    if launch.resume == token {
        return;
    }
    let op = Op::ResourceLaunchReported {
        resource: record.key,
        activation: activation.id,
        launch: LaunchSpec::Terminal(TerminalLaunch {
            resume: token,
            ..launch.clone()
        }),
    };
    if let Err(refusal) = s.commit_system(op) {
        tracing::debug!(?terminal, %refusal, "resume token not recorded");
    }
}

/// The argv that resumes `launch`'s program: the user's command for the
/// app that reported the token, with the token as whole arguments. `None`
/// (the recorded launch runs as before) without a token, without an entry
/// for its app, for an agent session, which resumes through its
/// integration, and for a stored token that is no longer well formed.
pub(super) fn argv(s: &ServerState, launch: &TerminalLaunch) -> Option<Vec<String>> {
    let resume = launch.resume.as_ref().filter(|resume| resume.is_valid())?;
    if launch.restore.is_some() {
        return None;
    }
    s.restore().resume_argv(&resume.app, &resume.token)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use phux_core::{LaunchSpec, ResumeToken, TerminalLaunch};

    use super::{argv, report};
    use crate::state::SharedState;

    fn stored(state: &SharedState, pane: phux_core::ids::ResourceId) -> TerminalLaunch {
        state.with(
            |s| match s.registry().record(pane).expect("record").launch {
                Some(LaunchSpec::Terminal(ref launch)) => launch.clone(),
                ref other => panic!("terminal launch: {other:?}"),
            },
        )
    }

    #[test]
    fn a_reported_token_lands_on_the_launch_record_and_resume_empty_clears_it() {
        let state = SharedState::new();
        let (_session, pane) = state.with_mut(|s| s.seed_session("resume"));
        let terminal = state.with_mut(|s| s.intern_resource_wire(pane));
        let before = stored(&state, pane);
        assert_eq!(before.resume, None);

        let token = ResumeToken::new("demo", "abc123");
        state.with_mut(|s| report(s, &terminal, token.clone()));
        assert_eq!(
            stored(&state, pane),
            TerminalLaunch {
                resume: token.clone(),
                ..before.clone()
            },
            "only the token changes"
        );
        // Not metadata, and not on the wire: no client can read it.
        state.with(|s| {
            let record = s.registry().record(pane).expect("record");
            let wire = crate::org_wire::launch(record.launch.as_ref().expect("launch"));
            assert!(!format!("{wire:?}").contains("abc123"));
            let scope = phux_protocol::wire::frame::Scope::Resource(terminal.clone());
            for key in s.metadata().list(&scope) {
                let value = s.metadata().get(&scope, &key).unwrap_or_default();
                assert!(!String::from_utf8_lossy(&value).contains("abc123"), "{key}");
            }
        });

        state.with_mut(|s| report(s, &terminal, None));
        assert_eq!(stored(&state, pane), before);

        state.with_mut(|s| {
            s.flush_reports_before_shutdown();
            report(s, &terminal, token);
        });
        assert_eq!(
            stored(&state, pane),
            before,
            "a stopping server records nothing"
        );
    }

    #[test]
    fn relaunch_runs_the_allowlisted_argv_and_falls_back_without_an_entry() {
        let state = SharedState::new();
        let launch = |app: &str, token: &str| TerminalLaunch {
            argv: vec!["recorded".to_owned()],
            resume: Some(ResumeToken {
                app: app.to_owned(),
                token: token.to_owned(),
            }),
            ..TerminalLaunch::default()
        };
        state.with(|s| assert_eq!(argv(s, &launch("demo", "abc123")), None, "no entry"));
        state.with_mut(|s| {
            s.set_restore(phux_config::RestoreCfg {
                on_start: false,
                program: vec![phux_config::RestoreProgram {
                    app: "demo".to_owned(),
                    command: ["demo", "--resume", phux_config::RESUME_TOKEN_PLACEHOLDER]
                        .map(str::to_owned)
                        .to_vec(),
                }],
            });
        });
        state.with(|s| {
            assert_eq!(
                argv(s, &launch("demo", "abc123")),
                Some(["demo", "--resume", "abc123"].map(str::to_owned).to_vec())
            );
            let command = super::super::launch::spec_command(s, &launch("demo", "abc123"));
            assert_eq!(*command.get_argv(), ["demo", "--resume", "abc123"]);
            let command = super::super::launch::spec_command(s, &launch("other", "abc123"));
            assert_eq!(*command.get_argv(), ["recorded"], "no entry for this app");
            // The store is a file: a token edited there is checked again.
            for edited in ["--help", "a b", "$(id)", ""] {
                assert_eq!(argv(s, &launch("demo", edited)), None, "{edited:?}");
            }
            let agent = TerminalLaunch {
                restore: Some(b"session".to_vec()),
                ..launch("demo", "abc123")
            };
            assert_eq!(
                argv(s, &agent),
                None,
                "an agent session resumes its own way"
            );
            assert_eq!(argv(s, &TerminalLaunch::default()), None);
        });
    }
}
