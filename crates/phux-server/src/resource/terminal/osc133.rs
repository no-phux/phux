//! Incremental OSC scanner for prompt marks and progress reports.
//!
//! Sources `command_started`/`command_finished` (SPEC §7.1) from the raw PTY
//! stream, because libghostty does not keep the `OSC 133 ; D ; <code>` exit
//! status. Stateful, so marks split across chunks still parse.
//!
//! * `OSC 133 ; A` / `B` → [`OscMark::PromptStart`] / [`OscMark::InputStart`]
//!   (prompt state only).
//! * `OSC 133 ; C` → [`OscMark::CommandStart`], the only `command_started`
//!   source.
//! * `OSC 133 ; D [; n]` → [`OscMark::CommandEnd`] with the optional code.
//! * `OSC 9 ; 4 ; …` → [`OscMark::Progress`] with the leading `9;` removed.
//!
//! [`PromptTracker`] folds marks into the `process.prompt` facet. Terminators
//! are BEL or ST; other OSCs pass through, and payloads over [`MAX_OSC_LEN`]
//! are consumed and dropped.

/// A recognised OSC mark consumed by the terminal actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OscMark {
    /// `OSC 133 ; A` — the shell began drawing a prompt.
    PromptStart,
    /// `OSC 133 ; B` — the prompt ended; the shell is reading input.
    InputStart,
    /// `OSC 133 ; C` — command execution began.
    CommandStart,
    /// `OSC 133 ; D [; code]`, with the exit code when parseable.
    CommandEnd {
        /// Exit code from `OSC 133 ; D ; n`, or `None` when absent/bogus.
        exit_code: Option<i32>,
    },
    /// `OSC 9 ; 4 ; ...` — ConEmu-style progress payload, without `9;`.
    Progress(String),
}

/// Longest OSC payload buffered; recognised marks are a few bytes.
const MAX_OSC_LEN: usize = 64;

/// Scanner state between chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Plain stream bytes.
    Ground,
    /// Saw `ESC`, deciding what follows.
    Escape,
    /// Inside `OSC` (`ESC ]`), collecting payload bytes into `buf`.
    Collect,
    /// Inside `OSC`, saw `ESC` — an `ST` (`ESC \`) terminator or an abort.
    CollectEscape,
}

/// Incremental scanner, one per pane; feed chunks in arrival order.
#[derive(Debug)]
pub(super) struct Osc133Scanner {
    state: State,
    /// Collected OSC payload (bytes between `ESC ]` and the terminator).
    buf: Vec<u8>,
    /// Over [`MAX_OSC_LEN`]: consume to the terminator, yield nothing.
    overflow: bool,
}

impl Osc133Scanner {
    /// A fresh scanner at ground state.
    pub(super) const fn new() -> Self {
        Self {
            state: State::Ground,
            buf: Vec::new(),
            overflow: false,
        }
    }

    /// Feed one chunk; returns completed marks in stream order.
    pub(super) fn feed(&mut self, chunk: &[u8]) -> Vec<OscMark> {
        let mut marks = Vec::new();
        // In `Ground` only ESC matters, so `feed_bytes` skips plain runs with
        // `memchr`, re-arming on each return to `Ground`. Mid-OSC bytes are
        // payload and are walked.
        self.feed_bytes(chunk, &mut marks);
        marks
    }

    /// The state machine, index-based so the `Ground` skip can jump.
    fn feed_bytes(&mut self, chunk: &[u8], marks: &mut Vec<OscMark>) {
        let mut index = 0;
        while index < chunk.len() {
            if matches!(self.state, State::Ground) {
                let Some(escape) = memchr::memchr(0x1b, &chunk[index..]) else {
                    return;
                };
                index += escape;
            }
            let byte = chunk[index];
            index += 1;
            // An aborted OSC's aborting byte may start something new, so it
            // can be processed twice.
            loop {
                match self.state {
                    State::Ground => {
                        if byte == 0x1b {
                            self.state = State::Escape;
                        }
                    }
                    State::Escape => match byte {
                        b']' => {
                            self.state = State::Collect;
                            self.buf.clear();
                            self.overflow = false;
                        }
                        // ESC ESC stays in Escape; other sequences are ignored.
                        0x1b => {}
                        _ => self.state = State::Ground,
                    },
                    State::Collect => match byte {
                        // BEL terminator.
                        0x07 => {
                            if let Some(mark) = self.finish() {
                                marks.push(mark);
                            }
                        }
                        0x1b => self.state = State::CollectEscape,
                        _ => {
                            if self.buf.len() < MAX_OSC_LEN {
                                self.buf.push(byte);
                            } else {
                                self.overflow = true;
                            }
                        }
                    },
                    State::CollectEscape => {
                        if byte == b'\\' {
                            // ST terminator.
                            if let Some(mark) = self.finish() {
                                marks.push(mark);
                            }
                        } else {
                            // ESC not forming ST aborts the OSC and starts a
                            // new sequence: re-process this byte.
                            self.buf.clear();
                            self.state = State::Escape;
                            continue;
                        }
                    }
                }
                break;
            }
        }
    }

    /// End the in-flight OSC: parse a mark from the payload, back to ground.
    fn finish(&mut self) -> Option<OscMark> {
        self.state = State::Ground;
        let overflow = std::mem::take(&mut self.overflow);
        let buf = std::mem::take(&mut self.buf);
        if overflow {
            return None;
        }
        parse_osc(&buf)
    }
}

/// The prompt state machine, fed every mark. Pure state, so it runs whether
/// or not anyone subscribes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct PromptTracker {
    facet: phux_core::process::PromptFacet,
}

impl PromptTracker {
    /// Fold one mark into the state.
    pub(super) const fn observe(&mut self, mark: &OscMark) {
        use phux_core::process::PromptState;
        match mark {
            OscMark::PromptStart | OscMark::InputStart => {
                self.facet.state = PromptState::AtPrompt;
            }
            OscMark::CommandStart => self.facet.state = PromptState::Running,
            OscMark::CommandEnd { exit_code } => {
                self.facet.state = PromptState::AtPrompt;
                self.facet.last_exit_code = *exit_code;
            }
            OscMark::Progress(_) => {}
        }
    }

    /// The current facet.
    pub(super) const fn facet(&self) -> phux_core::process::PromptFacet {
        self.facet
    }
}

/// Parse a complete OSC payload; `None` for marks this actor does not consume.
fn parse_osc(payload: &[u8]) -> Option<OscMark> {
    if let Some(progress) = payload.strip_prefix(b"9;") {
        if progress.starts_with(b"4;") {
            return String::from_utf8(progress.to_vec())
                .ok()
                .map(OscMark::Progress);
        }
        return None;
    }

    let rest = payload.strip_prefix(b"133;")?;
    let (kind, params) = match rest.split_first()? {
        (kind, []) => (*kind, None),
        (kind, params) => (*kind, params.strip_prefix(b";")),
    };
    match kind {
        b'A' => Some(OscMark::PromptStart),
        b'B' => Some(OscMark::InputStart),
        b'C' => Some(OscMark::CommandStart),
        b'D' => {
            // First parameter only; a bad code degrades to `None`.
            let exit_code = params.and_then(|params| {
                let first = params.split(|&b| b == b';').next()?;
                std::str::from_utf8(first).ok()?.parse::<i32>().ok()
            });
            Some(OscMark::CommandEnd { exit_code })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<OscMark> {
        let mut scanner = Osc133Scanner::new();
        let mut marks = Vec::new();
        for chunk in chunks {
            marks.extend(scanner.feed(chunk));
        }
        marks
    }

    #[test]
    fn d_mark_with_exit_code_bel_terminated() {
        assert_eq!(
            scan(&[b"prompt\x1b]133;D;0\x07more"]),
            vec![OscMark::CommandEnd { exit_code: Some(0) }]
        );
        assert_eq!(
            scan(&[b"\x1b]133;D;127\x07"]),
            vec![OscMark::CommandEnd {
                exit_code: Some(127)
            }]
        );
    }

    #[test]
    fn d_mark_st_terminated() {
        assert_eq!(
            scan(&[b"\x1b]133;D;1\x1b\\"]),
            vec![OscMark::CommandEnd { exit_code: Some(1) }]
        );
    }

    #[test]
    fn d_mark_without_code_is_none() {
        assert_eq!(
            scan(&[b"\x1b]133;D\x07"]),
            vec![OscMark::CommandEnd { exit_code: None }]
        );
    }

    #[test]
    fn bogus_code_degrades_to_none() {
        assert_eq!(
            scan(&[b"\x1b]133;D;nope\x07"]),
            vec![OscMark::CommandEnd { exit_code: None }]
        );
    }

    #[test]
    fn a_full_cycle_yields_each_mark_once_and_only_c_starts_a_command() {
        // A full A/B/C/D cycle yields exactly one start and one end.
        let marks =
            scan(&[b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07"]);
        assert_eq!(
            marks,
            vec![
                OscMark::PromptStart,
                OscMark::InputStart,
                OscMark::CommandStart,
                OscMark::CommandEnd { exit_code: Some(0) }
            ]
        );
        assert_eq!(
            marks
                .iter()
                .filter(|m| matches!(m, OscMark::CommandStart))
                .count(),
            1
        );
    }

    #[test]
    fn a_and_b_marks_tolerate_parameters() {
        assert_eq!(
            scan(&[b"\x1b]133;A;aid=7\x07\x1b]133;B;k=i\x1b\\"]),
            vec![OscMark::PromptStart, OscMark::InputStart]
        );
    }

    /// The prompt machine through a cycle with every mark split across
    /// chunks; a code-less `D` clears the stale code.
    #[test]
    fn prompt_state_tracks_a_c_d_and_survives_split_marks() {
        use phux_core::process::PromptState;

        let mut scanner = Osc133Scanner::new();
        let mut tracker = PromptTracker::default();
        let mut feed = |chunk: &[u8], tracker: &mut PromptTracker| {
            for mark in scanner.feed(chunk) {
                tracker.observe(&mark);
            }
        };
        assert_eq!(tracker.facet().state, PromptState::Unknown);

        feed(b"motd\x1b]13", &mut tracker);
        assert_eq!(
            tracker.facet().state,
            PromptState::Unknown,
            "mark incomplete"
        );
        feed(b"3;A\x07$ ", &mut tracker);
        assert_eq!(tracker.facet().state, PromptState::AtPrompt);

        feed(b"make\r\n\x1b]133", &mut tracker);
        feed(b";C\x07building", &mut tracker);
        assert_eq!(tracker.facet().state, PromptState::Running);

        feed(b"\x1b]133;D;", &mut tracker);
        assert_eq!(tracker.facet().state, PromptState::Running, "D incomplete");
        feed(b"3\x07", &mut tracker);
        assert_eq!(tracker.facet().state, PromptState::AtPrompt);
        assert_eq!(tracker.facet().last_exit_code, Some(3));

        feed(b"\x1b]133;C\x07\x1b]133;D\x07", &mut tracker);
        assert_eq!(tracker.facet().state, PromptState::AtPrompt);
        assert_eq!(
            tracker.facet().last_exit_code,
            None,
            "a code-less D clears it"
        );
    }

    #[test]
    fn mark_split_across_chunks_is_recognised() {
        // The whole point of statefulness: the OSC arrives in three reads.
        assert_eq!(
            scan(&[b"abc\x1b]13", b"3;D;", b"42\x07xyz"]),
            vec![OscMark::CommandEnd {
                exit_code: Some(42)
            }]
        );
    }

    /// The fast skip does not drop a mark whose payload chunk has no ESC.
    #[test]
    fn a_control_free_middle_chunk_is_still_scanned_as_osc_payload() {
        assert_eq!(
            scan(&[b"plain text\x1b]133;", b"D;7", b"\x07 more plain text"]),
            vec![OscMark::CommandEnd { exit_code: Some(7) }]
        );
        let mut chunk = vec![b'x'; 100_000];
        chunk.extend_from_slice(b"\x1b]133;C\x07");
        assert_eq!(scan(&[&chunk]), vec![OscMark::CommandStart]);
    }

    /// The skip re-arms after each mark: two marks with a long run between
    /// are both found.
    #[test]
    fn the_skip_rearms_after_each_return_to_ground() {
        let mut chunk = vec![b'x'; 100_000];
        chunk.extend_from_slice(b"\x1b]133;C\x07");
        chunk.extend(std::iter::repeat_n(b'y', 100_000));
        chunk.extend_from_slice(b"\x1b]133;D;0\x07");
        chunk.extend(std::iter::repeat_n(b'z', 100_000));
        let mut scanner = Osc133Scanner::new();
        assert_eq!(
            scanner.feed(&chunk),
            vec![
                OscMark::CommandStart,
                OscMark::CommandEnd { exit_code: Some(0) }
            ]
        );
        assert_eq!(scanner.state, State::Ground);
        // A CSI between plain runs (Escape -> Ground) re-arms it as well.
        assert_eq!(
            scan(&[b"aaaa\x1b[31mbbbb\x1b]133;C\x07cccc"]),
            vec![OscMark::CommandStart]
        );
    }

    /// A chunk with no ESC in `Ground` yields nothing and stays in `Ground`.
    #[test]
    fn a_control_free_chunk_in_ground_is_a_no_op() {
        let mut scanner = Osc133Scanner::new();
        assert_eq!(scanner.feed(&vec![b'a'; 8192]), Vec::new());
        assert_eq!(scanner.state, State::Ground);
        assert_eq!(scanner.feed(b"\x1b]133;C\x07"), vec![OscMark::CommandStart]);
    }

    #[test]
    fn progress_marks_preserve_payload_for_bel_st_and_split_chunks() {
        assert_eq!(
            scan(&[b"\x1b]9;4;3;\x07", b"\x1b]9;4;0;\x1b\\"]),
            vec![
                OscMark::Progress("4;3;".to_owned()),
                OscMark::Progress("4;0;".to_owned()),
            ]
        );
        assert_eq!(
            scan(&[b"\x1b]9;", b"4;3", b";\x07"]),
            vec![OscMark::Progress("4;3;".to_owned())]
        );
    }

    #[test]
    fn foreign_osc_9_and_invalid_progress_are_ignored() {
        assert_eq!(scan(&[b"\x1b]9;hello\x07\x1b]9;4;\xff\x07"]), Vec::new());
    }

    #[test]
    fn foreign_osc_and_other_escapes_yield_nothing() {
        assert_eq!(
            scan(&[b"\x1b]0;title\x07\x1b[31mred\x1b[0m\x1b]1337;x\x1b\\"]),
            Vec::new()
        );
    }

    #[test]
    fn overlong_osc_is_abandoned_and_bounded() {
        let mut payload = b"\x1b]133;D;".to_vec();
        payload.extend(std::iter::repeat_n(b'9', 4096));
        payload.push(0x07);
        let mut scanner = Osc133Scanner::new();
        assert_eq!(scanner.feed(&payload), Vec::new());
        assert!(scanner.buf.len() <= MAX_OSC_LEN, "buffer stays bounded");
        // And the scanner recovers: a following well-formed mark parses.
        assert_eq!(
            scanner.feed(b"\x1b]133;D;7\x07"),
            vec![OscMark::CommandEnd { exit_code: Some(7) }]
        );
    }

    #[test]
    fn esc_inside_osc_aborts_and_reprocesses() {
        // An OSC interrupted by a CSI yields nothing; a later mark parses.
        assert_eq!(
            scan(&[b"\x1b]133;D\x1b[31m\x1b]133;D;3\x07"]),
            vec![OscMark::CommandEnd { exit_code: Some(3) }]
        );
    }

    #[test]
    fn d_code_with_extra_params_takes_first() {
        assert_eq!(
            scan(&[b"\x1b]133;D;9;aid=42\x07"]),
            vec![OscMark::CommandEnd { exit_code: Some(9) }]
        );
    }
}
