//! The `resume=` key of OSC 7501: a token the program in the pane wants to
//! be relaunched with (`docs/program-protocol.md`, "Resume").
//!
//! The engine parses OSC 7501 but skips keys it does not know and reports
//! only fixed fields, so this reads `app` and `resume` out of the same PTY
//! bytes the engine is fed. The token goes to the server on its own
//! channel, never into a program-status record: every client can read
//! those.
//!
//! A token is recorded only when the foreground job is a direct child of
//! the pane shell, or the pane child itself when that child is the program.
//! The shell in the foreground, and a job that is not that child, are
//! ignored. OSC 133 `A` does not clear a token. The owning job's exit does.

// ponytail: a second parse of OSC 7501. Delete it for the engine's own
// report once libghostty exposes unknown keys.

use phux_core::ResumeToken;
use tokio::sync::watch;

use super::osc_scan::OscScan;

/// What follows `ESC ]` in a report.
const INTRO: &[u8] = b"7501;";
/// The protocol's 4096-byte sequence limit, less `OSC 7501 ;` and `ST`.
const MAX_BODY: usize = 4087;

/// What a write asks for: `Some(token)` hands one over, `None` withdraws it.
type Change = Option<ResumeToken>;

/// The scanner and the channel its findings leave on.
#[derive(Debug)]
pub(super) struct Resume {
    osc: OscScan,
    sink: Option<watch::Sender<Change>>,
    /// Foreground job that last set the token, so its exit can clear it
    /// while the shell is still alive.
    owner: Option<Owner>,
    /// Test double for the foreground-job queries. Production leaves this
    /// empty and reads the kernel at observe time.
    #[cfg(test)]
    test_job: Option<ResumeJob>,
}

impl Default for Resume {
    fn default() -> Self {
        Self {
            osc: OscScan::new(INTRO, MAX_BODY),
            sink: None,
            owner: None,
            #[cfg(test)]
            test_job: None,
        }
    }
}

impl Resume {
    /// The server's end. It starts empty and only changes count, so a pane
    /// rebuilt by an upgrade keeps the token its record already holds.
    pub(super) fn subscribe(&mut self) -> watch::Receiver<Change> {
        let (tx, rx) = watch::channel(None);
        self.sink = Some(tx);
        rx
    }

    /// Scan PTY output about to be written to the engine and publish the
    /// last change it asks for, when `job` says the foreground process may
    /// set a token. A failed check leaves the previous token in place.
    fn observe(&mut self, bytes: &[u8], job: ResumeJob) {
        if self.owner.is_some() && !job.owner_alive {
            self.owner = None;
            self.publish(None);
        }
        let Some(change) = self.scan(bytes) else {
            return;
        };
        if let Some(token) = change {
            if let Some(owner) = job.accepted_owner() {
                self.owner = Some(owner);
                self.publish(Some(token));
            }
        } else {
            self.owner = None;
            self.publish(None);
        }
    }

    /// The pane's process exited normally, so nothing is left to resume.
    /// Sent even with no token seen here: after an upgrade or a relaunch
    /// the record may still hold one. A prompt does not call this.
    pub(super) fn forget(&mut self) {
        self.owner = None;
        self.publish(None);
    }

    fn publish(&self, change: Change) {
        if let Some(sink) = &self.sink {
            sink.send_replace(change);
        }
    }

    /// The last change `bytes` asks for, continuing a report an earlier
    /// write left open.
    fn scan(&mut self, bytes: &[u8]) -> Option<Change> {
        let mut change = None;
        self.osc.scan(bytes, |body| {
            if let Some(next) = report(body) {
                change = Some(next);
            }
        });
        change
    }
}

/// What the finished body asks for. An empty `resume=` withdraws the
/// token. A token counts only with a well-formed `app` in the same
/// report; anything else changes nothing.
fn report(body: &[u8]) -> Option<Change> {
    let body = std::str::from_utf8(body).ok()?;
    let (mut app, mut resume) = (None, None);
    for pair in body.split(':') {
        match pair
            .split_once('=')
            .map(|(key, value)| (key.trim_ascii(), value.trim_ascii()))
        {
            Some(("app", value)) => app = Some(value),
            Some(("resume", value)) => resume = Some(value),
            _ => {}
        }
    }
    let resume = resume?;
    if resume.is_empty() {
        return Some(None);
    }
    ResumeToken::new(app?, resume).map(Some)
}

/// Foreground job that last handed over a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Owner {
    pid: i32,
    start_ms: u64,
}

/// What `observe` asks about the pane. Tests inject every field; production
/// fills them from the kernel. A missing pid, parent, or start time fails
/// closed, and so does any platform whose process helpers return `None`.
#[derive(Clone, Copy, Debug)]
struct ResumeJob {
    /// Foreground process-group leader (`tcgetpgrp`).
    fg_pgid: Option<i32>,
    /// Parent of that leader.
    parent: Option<i32>,
    /// Pane PTY child when it is a shell.
    shell_pid: Option<i32>,
    /// Pane PTY child when it is the program itself (no shell).
    direct_pid: Option<i32>,
    /// Start time of the foreground leader.
    start_ms: Option<u64>,
    /// Whether the recorded owner is still that process. A stopped job is
    /// still alive.
    owner_alive: bool,
}

impl ResumeJob {
    /// The owner a token may be recorded against, if this job may set one.
    fn accepted_owner(self) -> Option<Owner> {
        let fg = self.fg_pgid?;
        let start_ms = self.start_ms?;
        let under_shell = self
            .shell_pid
            .is_some_and(|shell| fg != shell && self.parent == Some(shell));
        let direct = self.shell_pid.is_none() && self.direct_pid == Some(fg);
        (under_shell || direct).then_some(Owner { pid: fg, start_ms })
    }
}

impl super::TerminalActor {
    /// Scan `bytes` for `resume=` before they are written to the engine.
    pub(super) fn observe_resume(&mut self, bytes: &[u8]) {
        let job = self.resume_job();
        self.resume.observe(bytes, job);
    }

    fn resume_job(&self) -> ResumeJob {
        #[cfg(test)]
        if let Some(job) = self.resume.test_job {
            return job;
        }
        self.live_resume_job()
    }

    /// Kernel answers for one observe. Unreadable argv, parent, or start
    /// time accepts nothing.
    fn live_resume_job(&self) -> ResumeJob {
        let child = pane_child_pid(&self.process);
        let fg = pane_master_fd(&self.process).and_then(phux_resource::proc_query::foreground_pgid);
        let parent = fg.and_then(phux_resource::proc_query::process_parent);
        let start_ms = fg.and_then(|pid| leader_start_ms(&self.process, child, pid));
        let (shell_pid, direct_pid) = shell_and_direct(child);
        let owner_alive = self.resume.owner.is_none_or(|owner| {
            owner_still_alive(
                owner,
                child,
                phux_resource::proc_query::process_start_ms(owner.pid),
            )
        });
        ResumeJob {
            fg_pgid: fg,
            parent,
            shell_pid,
            direct_pid,
            start_ms,
            owner_alive,
        }
    }

    #[cfg(test)]
    const fn set_resume_job_for_test(&mut self, job: ResumeJob) {
        self.resume.test_job = Some(job);
    }
}

/// The pane child's exit is decided from its wait status: a normal exit
/// clears the token there, and a signal or an unknown end keeps it. Any
/// other owner's exit clears the token while the shell is still alive.
fn owner_still_alive(owner: Owner, child: Option<i32>, start_ms: Option<u64>) -> bool {
    Some(owner.pid) == child || start_ms == Some(owner.start_ms)
}

fn pane_child_pid(process: &super::process::TerminalProcess) -> Option<i32> {
    if let Some(pty) = process.pty.as_ref() {
        return pty
            .child
            .process_id()
            .and_then(|pid| i32::try_from(pid).ok());
    }
    process.released_child_pid
}

fn pane_master_fd(process: &super::process::TerminalProcess) -> Option<std::os::fd::RawFd> {
    let master = process.pty.as_ref()?.master.lock().ok()?;
    master.as_raw_fd()
}

fn leader_start_ms(
    process: &super::process::TerminalProcess,
    child: Option<i32>,
    pid: i32,
) -> Option<u64> {
    if child == Some(pid)
        && let Some(start) = process.child_start_ms
    {
        return Some(start);
    }
    phux_resource::proc_query::process_start_ms(pid)
}

/// `(shell pid, direct-exec pid)`. An unreadable argv is neither, so the
/// check fails closed.
fn shell_and_direct(child: Option<i32>) -> (Option<i32>, Option<i32>) {
    match child.and_then(child_is_shell) {
        Some(true) => (child, None),
        Some(false) => (None, child),
        None => (None, None),
    }
}

/// `Some(true)` for a shell argv0, `Some(false)` for any other program,
/// `None` when argv cannot be read.
fn child_is_shell(pid: i32) -> Option<bool> {
    const SHELLS: &[&str] = &[
        "ash", "bash", "csh", "dash", "elvish", "fish", "ksh", "nu", "pwsh", "sh", "tcsh", "zsh",
    ];
    let argv = phux_resource::proc_query::process_argv(pid)?;
    let name = phux_resource::proc_query::argv0_name(&argv)?;
    Some(SHELLS.contains(&name.as_str()))
}

#[cfg(test)]
const fn job_under_shell(owner_alive: bool) -> ResumeJob {
    ResumeJob {
        fg_pgid: Some(20),
        parent: Some(10),
        shell_pid: Some(10),
        direct_pid: None,
        start_ms: Some(20),
        owner_alive,
    }
}

#[cfg(test)]
const fn job_shell_foreground() -> ResumeJob {
    ResumeJob {
        fg_pgid: Some(10),
        parent: Some(1),
        shell_pid: Some(10),
        direct_pid: None,
        start_ms: Some(10),
        owner_alive: true,
    }
}

#[cfg(test)]
const fn job_grandchild() -> ResumeJob {
    ResumeJob {
        fg_pgid: Some(30),
        parent: Some(20),
        shell_pid: Some(10),
        direct_pid: None,
        start_ms: Some(30),
        owner_alive: true,
    }
}

#[cfg(test)]
const fn job_direct_exec() -> ResumeJob {
    ResumeJob {
        fg_pgid: Some(7),
        parent: Some(1),
        shell_pid: None,
        direct_pid: Some(7),
        start_ms: Some(7),
        owner_alive: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Option<Change> {
        let mut resume = Resume::default();
        chunks
            .iter()
            .fold(None, |last, chunk| resume.scan(chunk).or(last))
    }

    fn token(app: &str, token: &str) -> Option<Change> {
        ResumeToken::new(app, token).map(Some)
    }

    #[test]
    fn a_report_split_at_any_byte_hands_over_the_same_token() {
        for report in [
            &b"out\x1b]7501;state=working:app=nvim:resume=abc123\x1b\\more"[..],
            b"out\x1b]7501;state=working:app=nvim:resume=abc123\x07more",
        ] {
            assert_eq!(scan(&[report]), token("nvim", "abc123"));
            for at in 0..=report.len() {
                assert_eq!(
                    scan(&[&report[..at], &report[at..]]),
                    token("nvim", "abc123"),
                    "split at {at}"
                );
            }
            let bytes: Vec<&[u8]> = report.chunks(1).collect();
            assert_eq!(scan(&bytes), token("nvim", "abc123"));
        }
    }

    #[test]
    fn the_last_pair_wins_and_a_token_needs_an_app_in_the_same_report() {
        let cases: [(&[u8], Option<Change>); 12] = [
            (
                b"\x1b]7501;app=a:resume=one:resume=two\x07",
                token("a", "two"),
            ),
            (b"\x1b]7501;app=a:app=b:resume=one\x07", token("b", "one")),
            (b"\x1b]7501; app = a : resume = one \x07", token("a", "one")),
            (b"\x1b]7501;state=working:resume=one\x07", None),
            (b"\x1b]7501;app=my app:resume=one\x07", None),
            (b"\x1b]7501;app=a:resume=-rf\x07", None),
            (b"\x1b]7501;app=a:resume=a b\x07", None),
            (b"\x1b]7501;app=a:resume=$(id)\x07", None),
            (b"\x1b]7501;state=working:app=a\x07", None),
            (b"\x1b]7501;?\x07", None),
            // An empty value withdraws the token, with or without an app.
            (b"\x1b]7501;state=idle:resume=\x07", Some(None)),
            (b"\x1b]7501;app=a:resume=one:resume=\x1b\\", Some(None)),
        ];
        for (bytes, want) in cases {
            assert_eq!(scan(&[bytes]), want, "{:?}", String::from_utf8_lossy(bytes));
        }
    }

    #[test]
    fn a_later_report_replaces_an_earlier_one_and_a_bad_one_changes_nothing() {
        assert_eq!(
            scan(&[b"\x1b]7501;app=a:resume=one\x07\x1b]7501;app=a:resume=two\x07"]),
            token("a", "two")
        );
        assert_eq!(
            scan(&[
                b"\x1b]7501;app=a:resume=one\x07",
                b"\x1b]7501;app=a:resume=-x\x07"
            ]),
            token("a", "one")
        );
        assert_eq!(
            scan(&[b"\x1b]7501;app=a:resume=one\x07\x1b]7501;resume=\x07"]),
            Some(None)
        );
    }

    #[test]
    fn other_sequences_and_cancelled_or_oversized_reports_are_ignored() {
        for bytes in [
            &b"plain text, no escapes"[..],
            b"\x1b[31mred\x1b[0m\x1b]0;app=a:resume=one\x07",
            b"\x1b]133;A\x07\x1b]75010;app=a:resume=one\x07",
            b"\x1b]7501app=a:resume=one\x07",
            b"]7501;app=a:resume=one\x07",
            b"\x1b]7501;app=a:resume=one\x18\x07",
            b"\x1b]7501;app=a:resume=one\x1a\x1b\\",
            // An `ESC` that is not `ST` abandons the report.
            b"\x1b]7501;app=a:resume=one\x1b[0m\x07",
            b"\x1b]7501;app=a:resume=one",
        ] {
            assert_eq!(scan(&[bytes]), None, "{:?}", String::from_utf8_lossy(bytes));
        }
        // An abandoned report does not hide the next one.
        assert_eq!(
            scan(&[b"\x1b]7501;app=a:resume=one\x1b\x1b]7501;app=b:resume=two\x07"]),
            token("b", "two")
        );

        let report = |body_len: usize| {
            let mut bytes = b"\x1b]7501;app=a:resume=one:x=".to_vec();
            let pad = body_len - (bytes.len() - 7);
            bytes.extend(std::iter::repeat_n(b'a', pad));
            bytes.push(0x07);
            bytes
        };
        assert_eq!(scan(&[&report(MAX_BODY)]), token("a", "one"));
        assert_eq!(scan(&[&report(MAX_BODY + 1)]), None);
    }

    #[test]
    fn changes_leave_on_the_channel_and_forgetting_always_notifies() {
        let mut resume = Resume::default();
        let mut rx = resume.subscribe();
        assert!(!rx.has_changed().expect("open"));
        resume.observe(b"no report here", super::job_under_shell(true));
        assert!(!rx.has_changed().expect("open"));
        resume.observe(
            b"\x1b]7501;state=working:app=a:resume=one\x07",
            super::job_under_shell(true),
        );
        assert_eq!(*rx.borrow_and_update(), ResumeToken::new("a", "one"));
        resume.forget();
        assert!(rx.has_changed().expect("open"));
        assert_eq!(*rx.borrow_and_update(), None);
        // A pane that never saw a token still tells the server to forget.
        resume.forget();
        assert!(rx.has_changed().expect("open"));
    }

    /// The engine skips the key, so the report is still a status report.
    /// A prompt expires that record and does not drop the resume token.
    #[test]
    fn the_engine_ignores_the_key_and_a_prompt_keeps_the_token() {
        use super::super::effects::{self, Effect};
        use super::super::program_status::Update;

        let report = b"\x1b]7501;state=working:app=a:resume=one\x1b\\";
        let (seen, _) = effects::observe(&[report]);
        assert!(
            matches!(&seen[..], [Effect::ProgramStatus(Update::Put(record))]
                if record.app.as_deref() == Some("a")),
            "{seen:?}"
        );

        let mut actor = crate::TerminalActor::new(20, 5).expect("new").actor;
        actor.set_resume_job_for_test(super::job_under_shell(true));
        let mut rx = actor.subscribe_resume();
        let mut status = actor.subscribe_program_status();
        actor.feed_for_test(report);
        assert_eq!(*rx.borrow_and_update(), ResumeToken::new("a", "one"));
        let json = serde_json::to_string(&status.borrow().clone());
        assert!(!json.expect("records").contains("one"), "not in a record");
        assert!(!status.borrow().is_empty(), "the report is still a record");
        actor.feed_for_test(b"\x1b]133;A\x07");
        assert_eq!(*rx.borrow(), ResumeToken::new("a", "one"));
        assert!(
            status.borrow_and_update().is_empty(),
            "a prompt still expires the record"
        );
    }

    #[test]
    fn a_pane_child_exit_is_not_treated_as_the_owning_job_leaving() {
        let owner = super::Owner {
            pid: 7,
            start_ms: 1,
        };
        assert!(super::owner_still_alive(owner, Some(7), None));
        assert!(!super::owner_still_alive(owner, Some(10), None));
        assert!(super::owner_still_alive(owner, Some(10), Some(1)));
        assert!(!super::owner_still_alive(owner, Some(10), Some(2)));
    }

    #[test]
    fn the_test_binary_is_not_classified_as_a_shell() {
        let pid = i32::try_from(std::process::id()).expect("pid fits");
        assert_ne!(super::child_is_shell(pid), Some(true));
    }
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod resume_tests;
