//! Incremental scanner for prompt, progress, program-status, and reset marks.
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
//! * `OSC 7501 ; …` → [`OscMark::ProgramStatus`] or
//!   [`OscMark::ProgramStatusQuery`].
//! * `ESC c` → [`OscMark::Reset`] (RIS, not DECSTR).
//!
//! [`PromptTracker`] folds marks into the `process.prompt` facet. Terminators
//! are BEL or ST; other OSCs pass through, and whole sequences over
//! [`MAX_OSC_LEN`] are consumed and dropped.

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
    /// `OSC 7501 ; pairs` — the report body, without `7501;`.
    ProgramStatus(String),
    /// `OSC 7501 ; ?` — feature detection.
    ProgramStatusQuery,
    /// `ESC c` — full terminal reset.
    Reset,
    /// BEL in ground state, not a control-string terminator.
    Bell,
}

/// Longest whole OSC sequence, including `ESC ]` and BEL or ST.
const MAX_OSC_LEN: usize = 4096;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

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
    /// Inside DCS, APC, PM, or SOS: ignore payload until ST or cancellation.
    String,
    /// Saw ESC inside a non-OSC control string, possibly the ST terminator.
    StringEscape,
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
        self.feed_with_offsets(chunk)
            .into_iter()
            .map(|(_, mark)| mark)
            .collect()
    }

    /// Feed one chunk, retaining each sequence's end offset in this chunk.
    ///
    /// Offsets are immediately after BEL, ST, or RIS, including when the
    /// sequence started in a previous chunk. This lets the actor answer a
    /// query before feeding later device-attribute requests to the engine.
    pub(super) fn feed_with_offsets(&mut self, chunk: &[u8]) -> Vec<(usize, OscMark)> {
        let mut marks = Vec::new();
        let mut index = 0;
        while index < chunk.len() {
            if matches!(self.state, State::Ground) {
                let Some(escape) = memchr::memchr2(ESC, BEL, &chunk[index..]) else {
                    break;
                };
                index += escape;
            }
            if let Some(mark) = self.step(chunk[index]) {
                marks.push((index + 1, mark));
            }
            index += 1;
        }
        marks
    }

    /// Advance the state machine by one byte.
    fn step(&mut self, byte: u8) -> Option<OscMark> {
        if matches!(byte, CAN | SUB) && !matches!(self.state, State::Ground) {
            self.state = State::Ground;
            self.buf.clear();
            self.overflow = false;
            return None;
        }
        match self.state {
            State::Ground => {
                if byte == ESC {
                    self.state = State::Escape;
                } else if byte == BEL {
                    return Some(OscMark::Bell);
                }
                None
            }
            State::Escape => self.on_escape(byte),
            State::Collect => match byte {
                BEL => self.finish(1),
                ESC => {
                    self.state = State::CollectEscape;
                    None
                }
                _ => {
                    self.collect(byte);
                    None
                }
            },
            State::CollectEscape => {
                if byte == b'\\' {
                    return self.finish(2);
                }
                // A non-ST ESC aborts the OSC. Reprocess the aborting byte
                // from Escape, so ESC ] and RIS are not lost.
                self.buf.clear();
                self.overflow = false;
                self.state = State::Escape;
                self.on_escape(byte)
            }
            State::String => {
                if byte == ESC {
                    self.state = State::StringEscape;
                }
                None
            }
            State::StringEscape => {
                self.state = match byte {
                    b'\\' => State::Ground,
                    ESC => State::StringEscape,
                    _ => State::String,
                };
                None
            }
        }
    }

    fn on_escape(&mut self, byte: u8) -> Option<OscMark> {
        match byte {
            b']' => {
                self.state = State::Collect;
                self.buf.clear();
                self.overflow = false;
            }
            b'c' => {
                self.state = State::Ground;
                return Some(OscMark::Reset);
            }
            b'P' | b'_' | b'^' | b'X' => self.state = State::String,
            // ESC ESC stays in Escape; other sequences are ignored.
            ESC => {}
            _ => self.state = State::Ground,
        }
        None
    }

    /// Reserve room for `ESC ]` and the shorter terminator (BEL).
    fn collect(&mut self, byte: u8) {
        if self.buf.len() < MAX_OSC_LEN - 3 {
            self.buf.push(byte);
        } else {
            self.overflow = true;
        }
    }

    /// End the in-flight OSC without giving up the buffer's allocation.
    fn finish(&mut self, terminator_len: usize) -> Option<OscMark> {
        self.state = State::Ground;
        let mark = if self.overflow || self.buf.len() + 2 + terminator_len > MAX_OSC_LEN {
            None
        } else {
            parse_osc(&self.buf)
        };
        self.buf.clear();
        self.overflow = false;
        mark
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
            OscMark::Progress(_)
            | OscMark::ProgramStatus(_)
            | OscMark::ProgramStatusQuery
            | OscMark::Reset
            | OscMark::Bell => {}
        }
    }

    /// The current facet.
    pub(super) const fn facet(&self) -> phux_core::process::PromptFacet {
        self.facet
    }
}

/// Parse a complete OSC payload; `None` for marks this actor does not consume.
fn parse_osc(payload: &[u8]) -> Option<OscMark> {
    if let Some(body) = payload.strip_prefix(b"7501;") {
        return if body == b"?" {
            Some(OscMark::ProgramStatusQuery)
        } else {
            report_body(body).map(OscMark::ProgramStatus)
        };
    }

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

/// Keep malformed byte pairs skippable, without expanding the sequence's size.
///
/// Invalid UTF-8 bytes become `!`, outside both key and value character sets.
/// This preserves hard byte caps, while valid UTF-8 (including whitespace)
/// stays untouched. Rejecting the entire OSC would lose unrelated valid pairs.
fn report_body(body: &[u8]) -> Option<String> {
    match String::from_utf8(body.to_vec()) {
        Ok(body) => Some(body),
        Err(error) => {
            let mut invalid = error.utf8_error();
            let mut bytes = error.into_bytes();
            let mut offset = 0;
            loop {
                let start = offset + invalid.valid_up_to();
                let end = start + invalid.error_len().unwrap_or(bytes.len() - start);
                bytes[start..end].fill(b'!');
                offset = end;
                match std::str::from_utf8(&bytes[offset..]) {
                    Ok(_) => break,
                    Err(error) => invalid = error,
                }
            }
            String::from_utf8(bytes).ok()
        }
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
    fn marks_parse_across_terminators_splits_and_noise() {
        let end = |code| vec![OscMark::CommandEnd { exit_code: code }];
        let progress = |p: &str| OscMark::Progress(p.to_owned());
        let cases: Vec<(&[&[u8]], Vec<OscMark>)> = vec![
            (&[b"prompt\x1b]133;D;0\x07more"], end(Some(0))),
            (&[b"\x1b]133;D;127\x07"], end(Some(127))),
            (&[b"\x1b]133;D;1\x1b\\"], end(Some(1))),
            (&[b"\x1b]133;D\x07"], end(None)),
            (&[b"\x1b]133;D;nope\x07"], end(None)),
            (&[b"\x1b]133;D;9;aid=42\x07"], end(Some(9))),
            (&[b"abc\x1b]13", b"3;D;", b"42\x07xyz"], end(Some(42))),
            // An OSC interrupted by a CSI yields nothing; a later mark parses.
            (&[b"\x1b]133;D\x1b[31m\x1b]133;D;3\x07"], end(Some(3))),
            // The aborting byte is re-processed: `ESC ]` both aborts the
            // open OSC and opens the next one, even split across chunks.
            (&[b"\x1b]133;D\x1b]133;D;5\x07"], end(Some(5))),
            (&[b"\x1b]133;D\x1b", b"]133;D;6\x07"], end(Some(6))),
            (
                &[b"\x1b]133;A;aid=7\x07\x1b]133;B;k=i\x1b\\"],
                vec![OscMark::PromptStart, OscMark::InputStart],
            ),
            (
                &[b"\x1b]9;4;3;\x07", b"\x1b]9;4;0;\x1b\\"],
                vec![progress("4;3;"), progress("4;0;")],
            ),
            (&[b"\x1b]9;", b"4;3", b";\x07"], vec![progress("4;3;")]),
            (&[b"\x1b]9;hello\x07\x1b]9;4;\xff\x07"], vec![]),
            (
                &[b"\x1b]0;title\x07\x1b[31mred\x1b[0m\x1b]1337;x\x1b\\"],
                vec![],
            ),
        ];
        for (chunks, want) in cases {
            assert_eq!(scan(chunks), want, "{chunks:?}");
        }
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
    fn program_status_queries_reports_and_ris_survive_every_chunk_split() {
        let bytes =
            b"noise\x1b]7501;?\x07\x1b]7501;state=blocked:id=build/test\x1b\\\x1bc\x1b]133;A\x07";
        let expected = vec![
            OscMark::ProgramStatusQuery,
            OscMark::ProgramStatus("state=blocked:id=build/test".to_owned()),
            OscMark::Reset,
            OscMark::PromptStart,
        ];
        for split in 0..=bytes.len() {
            assert_eq!(
                scan(&[&bytes[..split], &bytes[split..]]),
                expected,
                "{split}"
            );
        }
        let chunks: Vec<_> = bytes.chunks(1).collect();
        assert_eq!(scan(&chunks), expected);
        assert_eq!(
            scan(&[b"\x1b]7501;?;future=x\x07\x1b]7501;state=\xff\x07"]),
            vec![
                OscMark::ProgramStatus("?;future=x".to_owned()),
                OscMark::ProgramStatus("state=!".to_owned()),
            ]
        );
    }

    #[test]
    fn end_offsets_are_chunk_relative_exact_and_preserve_query_order() {
        let first = b"plain\x1b]7501;?\x07";
        let second = b"\x1b[c\x1b]7501;state=done\x1b\\";
        let third = b"\x1bc";
        let mut bytes = first.to_vec();
        bytes.extend_from_slice(second);
        bytes.extend_from_slice(third);
        let expected = vec![
            (first.len(), OscMark::ProgramStatusQuery),
            (
                first.len() + second.len(),
                OscMark::ProgramStatus("state=done".to_owned()),
            ),
            (bytes.len(), OscMark::Reset),
        ];
        for split in 0..=bytes.len() {
            let mut scanner = Osc133Scanner::new();
            let mut marks = scanner.feed_with_offsets(&bytes[..split]);
            marks.extend(
                scanner
                    .feed_with_offsets(&bytes[split..])
                    .into_iter()
                    .map(|(offset, mark)| (split + offset, mark)),
            );
            assert_eq!(marks, expected, "{split}");
        }
        let mut scanner = Osc133Scanner::new();
        assert!(scanner.feed_with_offsets(b"\x1b]7501;?\x1b").is_empty());
        assert_eq!(
            scanner.feed_with_offsets(b"\\\x1b[c"),
            vec![(1, OscMark::ProgramStatusQuery)]
        );
    }

    #[test]
    fn osc_cap_counts_the_whole_sequence_and_exact_terminator_length() {
        for terminator in [b"\x07".as_slice(), b"\x1b\\".as_slice()] {
            for excess in [0, 1] {
                let mut bytes = b"\x1b]7501;state=working:x=".to_vec();
                bytes.resize(MAX_OSC_LEN + excess - terminator.len(), b'a');
                bytes.extend_from_slice(terminator);
                let body = std::str::from_utf8(&bytes[7..bytes.len() - terminator.len()])
                    .unwrap()
                    .to_owned();
                let expected = if excess == 0 {
                    vec![OscMark::ProgramStatus(body)]
                } else {
                    Vec::new()
                };
                // Split the OSC opener, last payload byte, and ST itself.
                for split in [1, 2, bytes.len() - 2, bytes.len() - 1, bytes.len()] {
                    assert_eq!(
                        scan(&[&bytes[..split], &bytes[split..]]),
                        expected,
                        "terminator={terminator:?}, excess={excess}, split={split}"
                    );
                }
            }
        }
        // This same payload fits with BEL, but ST makes it one byte too long.
        let mut bytes = b"\x1b]7501;state=done:x=".to_vec();
        bytes.resize(MAX_OSC_LEN - 1, b'a');
        let mut bel = bytes.clone();
        bel.push(BEL);
        assert_eq!(scan(&[&bel]).len(), 1);
        bytes.extend_from_slice(b"\x1b\\");
        assert!(scan(&[&bytes]).is_empty());
    }

    #[test]
    fn only_ris_resets_and_new_marks_do_not_change_the_prompt_facet() {
        assert_eq!(
            scan(&[
                b"\x1b[!p\x1b[?1049h\x1b[?1049l\x1b\x1bc\x1b]aborted\x1b",
                b"c",
            ]),
            vec![OscMark::Reset, OscMark::Reset]
        );
        let mut tracker = PromptTracker::default();
        tracker.observe(&OscMark::CommandEnd { exit_code: Some(7) });
        tracker.observe(&OscMark::CommandStart);
        let before = tracker.facet();
        for mark in [
            OscMark::ProgramStatus("state=done".to_owned()),
            OscMark::ProgramStatusQuery,
            OscMark::Reset,
        ] {
            tracker.observe(&mark);
            assert_eq!(tracker.facet(), before);
        }
    }

    #[test]
    fn cancellations_abort_queries_and_reports_at_every_chunk_split() {
        for cancel in [CAN, SUB] {
            let mut bytes = b"\x1b]7501;?".to_vec();
            bytes.push(cancel);
            bytes.extend_from_slice(b"\x07\x1b]7501;state=done");
            bytes.push(cancel);
            bytes.extend_from_slice(b"\x1b\\\x1b]7501;?\x07");
            for split in 0..=bytes.len() {
                assert_eq!(
                    scan(&[&bytes[..split], &bytes[split..]]),
                    vec![OscMark::Bell, OscMark::ProgramStatusQuery],
                    "cancel={cancel}, split={split}"
                );
            }
        }
    }

    #[test]
    fn non_osc_control_strings_do_not_leak_nested_marks() {
        for opener in *b"P_^X" {
            let mut bytes = vec![ESC, opener];
            bytes.extend_from_slice(b"payload\x1b]7501;?\x07\x1bc\x1b]133;C\x07\x1b\\");
            bytes.extend_from_slice(b"\x1b]7501;?\x07");
            for split in 0..=bytes.len() {
                assert_eq!(
                    scan(&[&bytes[..split], &bytes[split..]]),
                    vec![OscMark::ProgramStatusQuery],
                    "opener={opener}, split={split}"
                );
            }
            for cancel in [CAN, SUB] {
                assert_eq!(
                    scan(&[&[ESC, opener, b'x', cancel], b"\x1b]7501;?\x07"]),
                    vec![OscMark::ProgramStatusQuery]
                );
            }
        }
    }
}
