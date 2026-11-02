//! Shell-integration effects the canonical engine reports during `vt_write`.
//!
//! libghostty parses OSC 133 semantic prompts, OSC 9;4 progress, OSC 7501
//! program status (validated and decoded), and ground-state BEL, and reports
//! each through a callback. The callbacks only queue an [`Effect`]; the actor
//! drains the queue after the write, in stream order, because it cannot be
//! borrowed from inside the engine. The engine also answers the OSC 7501
//! support query itself, in byte order with its other replies.
//!
//! [`PromptTracker`] folds effects into the `process.prompt` facet.

use std::cell::RefCell;
use std::rc::Rc;

use libghostty_vt::Terminal as GhosttyTerminal;
use libghostty_vt::terminal::{
    ProgramStatusKind, ProgramStatusReport, ProgramStatusState, ProgressReport, ProgressState,
    SemanticPrompt, SemanticPromptKind,
};

use super::program_status::{Record, Update};

/// One effect the engine reported, in stream order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Effect {
    /// `OSC 133 ; A` — the shell began drawing a prompt.
    PromptStart,
    /// `OSC 133 ; B` — the prompt ended; the shell is reading input.
    InputStart,
    /// `OSC 133 ; C` — command execution began.
    CommandStart,
    /// `OSC 133 ; D [; code]`, with the exit code when the shell sent one.
    CommandEnd {
        /// Exit code from `OSC 133 ; D ; n`, or `None` when absent.
        exit_code: Option<i32>,
    },
    /// `OSC 9 ; 4 ; ...`, rendered as `4;<state>;<percent>` (the percent
    /// empty when the program sent none) for the `osc-progress` region.
    Progress(String),
    /// A valid OSC 7501 report. A full reset arrives as a root clear.
    ProgramStatus(Update),
    /// BEL in ground state, not a control-string terminator.
    Bell,
}

/// The queue the engine's callbacks fill during a write.
pub(super) type PendingEffects = Rc<RefCell<Vec<Effect>>>;

/// Register the callbacks that queue [`Effect`]s on `pending`.
pub(super) fn install(
    terminal: &mut GhosttyTerminal<'static, 'static>,
    pending: &PendingEffects,
) -> Result<(), libghostty_vt::Error> {
    terminal
        .on_semantic_prompt({
            let pending = Rc::clone(pending);
            move |_term, event| {
                if let Some(effect) = semantic_prompt(event) {
                    pending.borrow_mut().push(effect);
                }
            }
        })?
        .on_progress_report({
            let pending = Rc::clone(pending);
            move |_term, report| {
                if let Some(effect) = progress(report) {
                    pending.borrow_mut().push(effect);
                }
            }
        })?
        .on_program_status({
            let pending = Rc::clone(pending);
            move |_term, report| {
                if let Some(update) = program_status(report) {
                    pending.borrow_mut().push(Effect::ProgramStatus(update));
                }
            }
        })?
        .on_bell({
            let pending = Rc::clone(pending);
            move |_term| pending.borrow_mut().push(Effect::Bell)
        })?;
    Ok(())
}

fn semantic_prompt(event: SemanticPrompt<'_>) -> Option<Effect> {
    Some(match event.kind().ok()? {
        SemanticPromptKind::PromptStart => Effect::PromptStart,
        SemanticPromptKind::InputStart => Effect::InputStart,
        SemanticPromptKind::OutputStart => Effect::CommandStart,
        SemanticPromptKind::CommandEnd => Effect::CommandEnd {
            exit_code: event.exit_code(),
        },
        _ => return None,
    })
}

fn progress(report: ProgressReport<'_>) -> Option<Effect> {
    let state = match report.state().ok()? {
        ProgressState::Remove => 0,
        ProgressState::Set => 1,
        ProgressState::Error => 2,
        ProgressState::Indeterminate => 3,
        ProgressState::Pause => 4,
        _ => return None,
    };
    let percent = report.progress().map(|p| p.to_string()).unwrap_or_default();
    Some(Effect::Progress(format!("4;{state};{percent}")))
}

fn program_status(report: ProgramStatusReport<'_>) -> Option<Update> {
    let id = report.id().to_owned();
    let state = report.state().ok()?;
    if state == ProgramStatusState::Clear {
        return Some(Update::Clear(id));
    }
    let text = |text: &str| (!text.is_empty()).then(|| text.to_owned());
    Some(Update::Put(Record {
        id,
        state: record_state(state)?,
        kind: report.kind().and_then(record_kind),
        progress: report.progress(),
        app: text(report.app()),
        title: text(report.title()),
        msg: text(report.message()),
    }))
}

/// The record's state name, or `None` for a state newer than this server.
const fn record_state(state: ProgramStatusState) -> Option<&'static str> {
    Some(match state {
        ProgramStatusState::Idle => "idle",
        ProgramStatusState::Working => "working",
        ProgramStatusState::Done => "done",
        ProgramStatusState::Blocked => "blocked",
        ProgramStatusState::Error => "error",
        _ => return None,
    })
}

/// The record's kind name, or `None` for a kind newer than this server.
const fn record_kind(kind: ProgramStatusKind) -> Option<&'static str> {
    Some(match kind {
        ProgramStatusKind::Permission => "permission",
        ProgramStatusKind::Question => "question",
        ProgramStatusKind::Auth => "auth",
        _ => return None,
    })
}

/// The prompt state machine, fed every effect. Pure state, so it runs
/// whether or not anyone subscribes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct PromptTracker {
    facet: phux_core::process::PromptFacet,
}

impl PromptTracker {
    /// Fold one effect into the state.
    pub(super) const fn observe(&mut self, effect: &Effect) {
        use phux_core::process::PromptState;
        match effect {
            Effect::PromptStart | Effect::InputStart => {
                self.facet.state = PromptState::AtPrompt;
            }
            Effect::CommandStart => self.facet.state = PromptState::Running,
            Effect::CommandEnd { exit_code } => {
                self.facet.state = PromptState::AtPrompt;
                self.facet.last_exit_code = *exit_code;
            }
            Effect::Progress(_) | Effect::ProgramStatus(_) | Effect::Bell => {}
        }
    }

    /// The current facet.
    pub(super) const fn facet(&self) -> phux_core::process::PromptFacet {
        self.facet
    }
}

/// Test-only: the effects a fresh engine reports for `chunks`, written in
/// order, with any replies it writes to the PTY.
#[cfg(test)]
pub(super) fn observe(chunks: &[&[u8]]) -> (Vec<Effect>, Vec<u8>) {
    let pending = PendingEffects::default();
    let replies = Rc::new(RefCell::new(Vec::new()));
    let mut terminal = GhosttyTerminal::new(80, 24).expect("terminal");
    install(&mut terminal, &pending).expect("effects");
    terminal
        .on_pty_write({
            let replies = Rc::clone(&replies);
            move |_term, bytes| replies.borrow_mut().extend_from_slice(bytes)
        })
        .expect("pty writer");
    for chunk in chunks {
        terminal.vt_write(chunk);
    }
    drop(terminal);
    let effects = pending.take();
    let replies = replies.take();
    (effects, replies)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effects(chunks: &[&[u8]]) -> Vec<Effect> {
        observe(chunks).0
    }

    #[test]
    fn semantic_prompts_and_progress_parse_across_terminators_and_splits() {
        let end = |code| vec![Effect::CommandEnd { exit_code: code }];
        let progress = |p: &str| Effect::Progress(p.to_owned());
        let cases: Vec<(&[&[u8]], Vec<Effect>)> = vec![
            (&[b"prompt\x1b]133;D;0\x07more"], end(Some(0))),
            (&[b"\x1b]133;D;127\x07"], end(Some(127))),
            (&[b"\x1b]133;D;1\x1b\\"], end(Some(1))),
            (&[b"\x1b]133;D\x07"], end(None)),
            (&[b"\x1b]133;D;9;aid=42\x07"], end(Some(9))),
            (&[b"abc\x1b]13", b"3;D;", b"42\x07xyz"], end(Some(42))),
            (
                &[b"\x1b]133;A;aid=7\x07\x1b]133;B;k=i\x1b\\"],
                vec![Effect::PromptStart, Effect::InputStart],
            ),
            (
                &[b"\x1b]9;4;3;\x07", b"\x1b]9;4;0;\x1b\\"],
                vec![progress("4;3;"), progress("4;0;")],
            ),
            (&[b"\x1b]9;", b"4;1", b";42\x07"], vec![progress("4;1;42")]),
            (
                &[b"\x1b]0;title\x07\x1b[31mred\x1b[0m\x1b]1337;x\x1b\\"],
                vec![],
            ),
        ];
        for (chunks, want) in cases {
            assert_eq!(effects(chunks), want, "{chunks:?}");
        }
    }

    /// The prompt machine through a cycle; a code-less `D` clears the stale
    /// code, and only `C` starts a command.
    #[test]
    fn prompt_state_tracks_a_full_cycle() {
        use phux_core::process::PromptState;

        let mut tracker = PromptTracker::default();
        assert_eq!(tracker.facet().state, PromptState::Unknown);
        let cycle = effects(&[
            b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D;3\x07",
        ]);
        assert_eq!(
            cycle,
            [
                Effect::PromptStart,
                Effect::InputStart,
                Effect::CommandStart,
                Effect::CommandEnd { exit_code: Some(3) },
            ]
        );
        for effect in &cycle[..3] {
            tracker.observe(effect);
        }
        assert_eq!(tracker.facet().state, PromptState::Running);
        tracker.observe(&cycle[3]);
        assert_eq!(tracker.facet().state, PromptState::AtPrompt);
        assert_eq!(tracker.facet().last_exit_code, Some(3));
        for effect in effects(&[b"\x1b]133;C\x07\x1b]133;D\x07"]) {
            tracker.observe(&effect);
        }
        assert_eq!(tracker.facet().last_exit_code, None);
    }

    #[test]
    fn only_ground_bel_rings() {
        assert_eq!(
            effects(&[
                b"\x1b]7501;state=working",
                b"\x07\x1b]99;ignored\x07\x1bPx\x07\x1b\\"
            ]),
            [Effect::ProgramStatus(Update::Put(Record {
                id: String::new(),
                state: "working",
                kind: None,
                progress: None,
                app: None,
                title: None,
                msg: None,
            }))]
        );
        assert_eq!(effects(&[b"\x07\x07"]), [Effect::Bell, Effect::Bell]);
    }

    /// The engine answers the support query in byte order with DA, and a
    /// cancelled query or report has no effect.
    #[test]
    fn the_engine_answers_the_support_query_before_a_following_da() {
        let (seen, replies) = observe(&[b"\x1b]7501;?\x1b\\\x1b[c"]);
        assert!(seen.is_empty());
        let da = replies
            .strip_prefix(b"\x1b]7501;?:states=")
            .and_then(|tail| {
                tail.windows(2)
                    .position(|w| w == b"\x1b\\")
                    .map(|end| &tail[end + 2..])
            })
            .expect("support reply first");
        assert!(
            da.starts_with(b"\x1b[?") && da.ends_with(b"c"),
            "{replies:?}"
        );

        for cancel in [0x18_u8, 0x1a] {
            let mut bytes = b"\x1b]7501;?".to_vec();
            bytes.push(cancel);
            bytes.extend_from_slice(b"\x1b]7501;state=done");
            bytes.push(cancel);
            let (seen, replies) = observe(&[&bytes]);
            assert!(seen.is_empty() && replies.is_empty(), "cancel={cancel}");
        }
    }

    /// Only RIS clears: it removes progress and every record, while a soft
    /// reset and an alternate-screen round trip keep both.
    #[test]
    fn a_full_reset_clears_progress_and_every_record() {
        assert_eq!(
            effects(&[b"\x1b[!p\x1b[?1049h\x1b[?1049l\x1bc"]),
            [
                Effect::Progress("4;0;".to_owned()),
                Effect::ProgramStatus(Update::Clear(String::new())),
            ]
        );
    }
}
