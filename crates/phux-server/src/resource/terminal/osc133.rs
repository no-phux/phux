//! Incremental OSC scanner for prompt marks and progress reports.
//!
//! Sources the `command_started` / `command_finished` agent events (SPEC
//! §7.1) directly from the raw PTY byte stream. libghostty records OSC-133
//! semantic marks per *cell* but does not retain the `OSC 133 ; D ; <code>`
//! exit status, so the grid projection cannot yield it — the honest source
//! is the byte stream the actor is already pumping. The scanner is a tiny
//! stateful machine fed one PTY chunk at a time, so a mark split across two
//! `read()` chunks is still recognised.
//!
//! Recognised marks (`FinalTerm` / iTerm2 shell-integration vocabulary):
//!
//! * `OSC 133 ; C …`  → [`OscMark::CommandStart`] — the shell is about
//!   to execute the typed command (output begins). `A` (prompt start) and
//!   `B` (input start) are accepted and ignored: emitting on `C` yields
//!   exactly one `command_started` per command, where `B` would double-fire.
//! * `OSC 133 ; D`     → [`OscMark::CommandEnd { exit_code: None }`].
//! * `OSC 133 ; D ; n` → [`OscMark::CommandEnd { exit_code: Some(n) }`].
//! * `OSC 9 ; 4 ; …`   → [`OscMark::Progress`] with the leading `9;` removed.
//!
//! Terminators: BEL (`0x07`) or ST (`ESC \`). Any other escape sequence and
//! every OSC except 133 and 9;4 passes through unrecognised. Payloads are
//! bounded: an OSC whose collected bytes exceed [`MAX_OSC_LEN`] is
//! abandoned (consumed to its terminator, yielding nothing), so a
//! pathological stream cannot grow the scanner's buffer.

/// A recognised OSC mark consumed by the terminal actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OscMark {
    /// `OSC 133 ; C` — command execution began.
    CommandStart,
    /// `OSC 133 ; D [; code]` — command finished, with the shell-reported
    /// exit code when present and parseable.
    CommandEnd {
        /// Exit code from `OSC 133 ; D ; n`, or `None` when absent/bogus.
        exit_code: Option<i32>,
    },
    /// `OSC 9 ; 4 ; ...` — ConEmu-style progress payload, without `9;`.
    Progress(String),
}

/// Longest OSC payload the scanner will buffer. Recognised prompt/progress
/// marks are a handful of bytes; anything longer is not ours.
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

/// Incremental scanner; one per pane actor. Feed every PTY chunk in
/// arrival order.
#[derive(Debug)]
pub(super) struct Osc133Scanner {
    state: State,
    /// Collected OSC payload (bytes between `ESC ]` and the terminator).
    buf: Vec<u8>,
    /// Payload exceeded [`MAX_OSC_LEN`]; consume to the terminator and
    /// yield nothing.
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

    /// Feed one PTY chunk; returns the prompt marks completed inside it,
    /// in stream order.
    pub(super) fn feed(&mut self, chunk: &[u8]) -> Vec<OscMark> {
        let mut marks = Vec::new();
        // Ground-state fast skip. In `Ground` the machine reacts to exactly
        // one byte, `ESC`; every other byte is a no-op that still costs a
        // match arm and a loop iteration. A `cat` of a plain-text file is
        // 100% such bytes, so stepping them one at a time made this scanner
        // one of the most expensive things the actor did per chunk —
        // ~0.5 ms/MB, comparable to libghostty's whole VT parse of the same
        // bytes. `memchr` is the SIMD form of the identical search; the state
        // machine below is entered unchanged from wherever it lands.
        //
        // The skip applies from `Ground` only, and it re-arms every time the
        // machine returns to `Ground` mid-chunk (phux-l96p.13): a chunk
        // shaped `[100 KB plain][mark][100 KB plain]` skips both runs, not
        // just the first. A scan resumed mid-OSC still walks every byte,
        // because those bytes are the OSC payload and a control-free chunk
        // can be the middle of a mark.
        self.feed_bytes(chunk, &mut marks);
        marks
    }

    /// The state machine proper: one pass over `chunk` from the current
    /// state, index-based so the `Ground` skip can jump ahead.
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
            // A byte may need re-processing after an aborted OSC (the
            // aborting byte is itself the start of something new), hence
            // the small loop.
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
                        // ESC ESC stays in Escape; anything else is some
                        // other sequence we do not track.
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
                            // ESC inside an OSC that is not ST aborts the
                            // OSC; the ESC starts a new sequence and THIS
                            // byte belongs to it — re-process it.
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

    /// Terminate the in-flight OSC: parse a 133 prompt mark out of the
    /// collected payload (or `None` for foreign / overflowed payloads)
    /// and return to ground.
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
        b'C' => Some(OscMark::CommandStart),
        b'D' => {
            // `133;D` alone, or `133;D;<code>[;...]` — take the first
            // parameter; a non-numeric or over-range code degrades to
            // `None` (the wire field is optional by design).
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
    fn c_mark_emits_command_start_but_a_and_b_do_not() {
        // A full shell-integration cycle: prompt (A), input (B), execute
        // (C), finish (D). Exactly one start and one end come out.
        assert_eq!(
            scan(&[b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\r\n\x1b]133;D;0\x07"]),
            vec![
                OscMark::CommandStart,
                OscMark::CommandEnd { exit_code: Some(0) }
            ]
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

    /// The ground-state fast skip must not change what the machine sees. A
    /// mark whose payload chunk contains no `ESC` at all is exactly the case
    /// the skip would wrongly discard if it applied outside `Ground`.
    #[test]
    fn a_control_free_middle_chunk_is_still_scanned_as_osc_payload() {
        assert_eq!(
            scan(&[b"plain text\x1b]133;", b"D;7", b"\x07 more plain text"]),
            vec![OscMark::CommandEnd { exit_code: Some(7) }]
        );
        // ... and a long run of plain bytes before the mark is skipped
        // without losing it.
        let mut chunk = vec![b'x'; 100_000];
        chunk.extend_from_slice(b"\x1b]133;C\x07");
        assert_eq!(scan(&[&chunk]), vec![OscMark::CommandStart]);
    }

    /// The skip re-arms after every return to `Ground` (phux-l96p.13): a
    /// chunk with two marks separated by a long plain run finds both, and
    /// the run between them is skipped rather than stepped. The state after
    /// the chunk is `Ground` again, so the next chunk skips too.
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

    /// A chunk with no `ESC` while in `Ground` yields nothing and leaves the
    /// scanner in `Ground` — the fast skip's contract.
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
        // An OSC interrupted by a CSI: the OSC yields nothing, and a
        // subsequent complete mark still parses (the aborting ESC's own
        // sequence is consumed correctly).
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
