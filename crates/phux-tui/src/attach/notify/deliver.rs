//! Turning a [`Note`] into something the user sees.
//!
//! Agent names, window names, and session names are written by agents and
//! other clients, and every byte of a note ends up inside an escape sequence
//! on the outer terminal. [`sanitize`] is that injection boundary: no control
//! character (ESC, BEL, the C1 `ST`) survives it, so a name cannot end the
//! OSC early and smuggle its own sequence onto the user's terminal.

use std::process::{Command, Stdio};

use phux_config::{NotificationsCfg, NotifyMode, NotifyProtocol};

use super::Note;

/// Longest title or body, in characters, before it is cut with an ellipsis.
pub(super) const MAX_FIELD_CHARS: usize = 160;

/// What the outer environment supports, read once per attach entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::attach) struct HostEnv {
    /// The dialect `terminal-protocol = "auto"` resolves to.
    pub auto_protocol: NotifyProtocol,
    /// The TUI runs inside tmux: OSC needs DCS passthrough wrapping.
    pub tmux: bool,
    /// The desktop notifier argv shape.
    pub macos: bool,
}

impl HostEnv {
    /// Read the process environment.
    pub(in crate::attach) fn detect() -> Self {
        Self::from_env(|key| std::env::var(key).ok())
    }

    /// [`Self::detect`] over an injectable lookup.
    pub(super) fn from_env(env: impl Fn(&str) -> Option<String>) -> Self {
        let term = env("TERM").unwrap_or_default();
        let program = env("TERM_PROGRAM").unwrap_or_default();
        let auto_protocol = if env("KITTY_WINDOW_ID").is_some() || term == "xterm-kitty" {
            NotifyProtocol::Osc99
        } else if program.eq_ignore_ascii_case("ghostty")
            || program.eq_ignore_ascii_case("wezterm")
            || term == "xterm-ghostty"
            || term.starts_with("foot")
        {
            NotifyProtocol::Osc777
        } else {
            NotifyProtocol::Osc9
        };
        Self {
            auto_protocol,
            tmux: env("TMUX").is_some_and(|value| !value.is_empty()),
            macos: cfg!(target_os = "macos"),
        }
    }
}

/// How one note reaches the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::attach) enum Delivery {
    /// Bytes for the outer terminal.
    Terminal(Vec<u8>),
    /// An argv for the desktop notifier, spawned without a shell.
    System(Vec<String>),
}

/// Plan the delivery of `note` under `cfg`; `None` when notifications are off.
pub(in crate::attach) fn plan(
    note: &Note,
    cfg: NotificationsCfg,
    env: HostEnv,
    seq: u32,
) -> Option<Delivery> {
    let title = sanitize(&note.title);
    let body = sanitize(&note.body);
    match cfg.mode {
        NotifyMode::Off => None,
        NotifyMode::Bell => Some(Delivery::Terminal(vec![0x07])),
        NotifyMode::System => Some(Delivery::System(system_argv(
            &title, &body, cfg.sound, env.macos,
        ))),
        NotifyMode::Terminal => {
            let protocol = match cfg.terminal_protocol {
                NotifyProtocol::Auto => env.auto_protocol,
                pinned => pinned,
            };
            let osc = match protocol {
                NotifyProtocol::Osc99 => osc99(&title, &body, seq),
                NotifyProtocol::Osc777 => osc777(&title, &body),
                NotifyProtocol::Auto | NotifyProtocol::Osc9 => osc9(&title, &body),
            };
            let mut bytes = if env.tmux { tmux_wrap(&osc) } else { osc };
            if cfg.sound {
                bytes.push(0x07);
            }
            Some(Delivery::Terminal(bytes))
        }
    }
}

/// Make agent-provided text safe inside an OSC string and as one argv word:
/// control characters (C0, DEL, C1, so ESC, BEL, and `ST`) are dropped, line
/// breaks and tabs become spaces, `;` (the OSC field separator) becomes `,`,
/// a leading `-` is dropped so the word never reads as a flag, and the
/// result is cut to [`MAX_FIELD_CHARS`].
pub(in crate::attach) fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_FIELD_CHARS * 4));
    let mut chars = 0usize;
    for c in text.chars() {
        let c = match c {
            '\n' | '\r' | '\t' => ' ',
            ';' => ',',
            c if c.is_control() => continue,
            c => c,
        };
        if chars == MAX_FIELD_CHARS {
            out.pop();
            out.push('\u{2026}');
            break;
        }
        out.push(c);
        chars += 1;
    }
    let trimmed = out.trim().trim_start_matches('-').trim_start();
    trimmed.to_owned()
}

/// `OSC 9 ; title: body ST`. A message that opens with a digit would read
/// as a `ConEmu` sub-command (`9;4;` is progress), so it gets a prefix.
pub(super) fn osc9(title: &str, body: &str) -> Vec<u8> {
    let mut message = if body.is_empty() {
        title.to_owned()
    } else {
        format!("{title}: {body}")
    };
    if message.starts_with(|c: char| c.is_ascii_digit()) {
        message.insert_str(0, "phux: ");
    }
    let mut out = b"\x1b]9;".to_vec();
    out.extend_from_slice(message.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

/// `OSC 777 ; notify ; title ; body ST`.
pub(super) fn osc777(title: &str, body: &str) -> Vec<u8> {
    let mut out = b"\x1b]777;notify;".to_vec();
    out.extend_from_slice(title.as_bytes());
    out.push(b';');
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

/// kitty's `OSC 99`: the title chunk, then the body chunk that completes
/// it, both base64 (`e=1`) so no payload byte is ever interpreted.
pub(super) fn osc99(title: &str, body: &str, seq: u32) -> Vec<u8> {
    let id = format!("phux{seq}");
    let mut out = Vec::new();
    for (meta, text) in [("d=0:p=title", title), ("d=1:p=body", body)] {
        out.extend_from_slice(format!("\x1b]99;i={id}:{meta}:e=1;").as_bytes());
        out.extend_from_slice(crate::attach::copy::base64_encode(text.as_bytes()).as_bytes());
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// Wrap `seq` in tmux's DCS passthrough (every ESC doubled). tmux forwards
/// it only with `allow-passthrough on`.
pub(super) fn tmux_wrap(seq: &[u8]) -> Vec<u8> {
    let mut out = b"\x1bPtmux;".to_vec();
    for &byte in seq {
        if byte == 0x1b {
            out.push(0x1b);
        }
        out.push(byte);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// The desktop notifier argv. macOS hands the text to `osascript` as `argv`
/// items of a fixed script, so no text is ever parsed as `AppleScript`.
fn system_argv(title: &str, body: &str, sound: bool, macos: bool) -> Vec<String> {
    if macos {
        let display = if sound {
            "display notification (item 2 of argv) with title (item 1 of argv) sound name \"Glass\""
        } else {
            "display notification (item 2 of argv) with title (item 1 of argv)"
        };
        return [
            "osascript",
            "-e",
            "on run argv",
            "-e",
            display,
            "-e",
            "end run",
            title,
            body,
        ]
        .map(str::to_owned)
        .to_vec();
    }
    let mut argv = vec!["notify-send".to_owned(), "--app-name=phux".to_owned()];
    if sound {
        argv.push("--hint=string:sound-name:message-new-instant".to_owned());
    }
    argv.extend(["--".to_owned(), title.to_owned(), body.to_owned()]);
    argv
}

/// Spawn the notifier without a shell and without waiting on it; a thread
/// reaps the child so it never lingers as a zombie. A missing notifier is
/// logged, never surfaced.
pub(in crate::attach) fn spawn_system(argv: &[String]) {
    let Some((program, rest)) = argv.split_first() else {
        return;
    };
    let spawned = Command::new(program)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            let _reaper = std::thread::Builder::new()
                .name("phux-notify".to_owned())
                .spawn(move || {
                    let _ = child.wait();
                });
        }
        Err(err) => tracing::debug!(%err, program, "desktop notifier unavailable"),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn note(title: &str, body: &str) -> Note {
        Note {
            title: title.to_owned(),
            body: body.to_owned(),
        }
    }

    fn env(protocol: NotifyProtocol) -> HostEnv {
        HostEnv {
            auto_protocol: protocol,
            tmux: false,
            macos: false,
        }
    }

    fn terminal(protocol: NotifyProtocol) -> NotificationsCfg {
        NotificationsCfg {
            terminal_protocol: protocol,
            ..NotificationsCfg::default()
        }
    }

    #[test]
    fn sanitize_strips_every_terminator_and_control() {
        let hostile = "evil\x1b]52;c;AAAA\x07\u{9c}\x1b\\\u{9b}31m\x7fname";
        let clean = sanitize(hostile);
        assert!(!clean.chars().any(char::is_control), "{clean:?}");
        assert!(!clean.contains(';'));
        assert_eq!(clean, "evil]52,c,AAAA\\31mname");
    }

    #[test]
    fn sanitize_flattens_lines_drops_flags_and_caps_length() {
        assert_eq!(sanitize("a\nb\tc"), "a b c");
        assert_eq!(sanitize("--rm -rf"), "rm -rf");
        let long = "x".repeat(MAX_FIELD_CHARS + 20);
        let cut = sanitize(&long);
        assert_eq!(cut.chars().count(), MAX_FIELD_CHARS);
        assert!(cut.ends_with('\u{2026}'));
        assert_eq!(
            sanitize(&"y".repeat(MAX_FIELD_CHARS)).chars().count(),
            MAX_FIELD_CHARS
        );
    }

    #[test]
    fn osc9_encodes_and_guards_a_leading_digit() {
        assert_eq!(
            osc9("claude finished", "api in dev"),
            b"\x1b]9;claude finished: api in dev\x1b\\"
        );
        assert_eq!(
            osc9("4 agents finished", ""),
            b"\x1b]9;phux: 4 agents finished\x1b\\"
        );
    }

    #[test]
    fn osc777_encodes_title_and_body() {
        assert_eq!(osc777("t", "b"), b"\x1b]777;notify;t;b\x1b\\");
    }

    #[test]
    fn osc99_sends_base64_title_then_body() {
        let out = osc99("hi", "yo", 7);
        assert_eq!(
            out,
            b"\x1b]99;i=phux7:d=0:p=title:e=1;aGk=\x1b\\\x1b]99;i=phux7:d=1:p=body:e=1;eW8=\x1b\\"
        );
    }

    #[test]
    fn tmux_wrap_doubles_escapes() {
        assert_eq!(
            tmux_wrap(b"\x1b]9;x\x1b\\"),
            b"\x1bPtmux;\x1b\x1b]9;x\x1b\x1b\\\x1b\\"
        );
    }

    #[test]
    fn plan_sanitizes_before_encoding() {
        let hostile = note("a\x1b\\\x1b]0;pwned\x07", "b");
        let Some(Delivery::Terminal(bytes)) = plan(
            &hostile,
            terminal(NotifyProtocol::Osc777),
            env(NotifyProtocol::Osc9),
            1,
        ) else {
            panic!("terminal delivery");
        };
        // Exactly one ESC opens and one ST closes; nothing in between.
        assert_eq!(bytes.first(), Some(&0x1b));
        assert!(bytes.ends_with(b"\x1b\\"));
        assert!(!bytes[1..bytes.len() - 2].contains(&0x1b));
        assert!(!bytes.contains(&0x07));
    }

    #[test]
    fn plan_picks_protocol_tmux_and_sound() {
        let n = note("t", "b");
        let auto = NotificationsCfg::default();
        assert_eq!(
            plan(&n, auto, env(NotifyProtocol::Osc777), 1),
            Some(Delivery::Terminal(osc777("t", "b")))
        );
        assert_eq!(
            plan(
                &n,
                terminal(NotifyProtocol::Osc9),
                env(NotifyProtocol::Osc777),
                1
            ),
            Some(Delivery::Terminal(osc9("t", "b"))),
            "a pinned dialect beats detection"
        );
        let tmux = HostEnv {
            tmux: true,
            ..env(NotifyProtocol::Osc9)
        };
        let loud = NotificationsCfg {
            sound: true,
            ..auto
        };
        let mut want = tmux_wrap(&osc9("t", "b"));
        want.push(0x07);
        assert_eq!(plan(&n, loud, tmux, 1), Some(Delivery::Terminal(want)));
    }

    #[test]
    fn plan_bell_off_and_system() {
        let n = note("t", "b");
        let bell = NotificationsCfg {
            mode: NotifyMode::Bell,
            ..NotificationsCfg::default()
        };
        assert_eq!(
            plan(&n, bell, env(NotifyProtocol::Osc9), 1),
            Some(Delivery::Terminal(vec![0x07]))
        );
        let off = NotificationsCfg {
            mode: NotifyMode::Off,
            ..NotificationsCfg::default()
        };
        assert_eq!(plan(&n, off, env(NotifyProtocol::Osc9), 1), None);
        let system = NotificationsCfg {
            mode: NotifyMode::System,
            ..NotificationsCfg::default()
        };
        let Some(Delivery::System(linux)) =
            plan(&note("-t", "b"), system, env(NotifyProtocol::Osc9), 1)
        else {
            panic!("system delivery");
        };
        assert_eq!(linux, ["notify-send", "--app-name=phux", "--", "t", "b"]);
        let mac = HostEnv {
            macos: true,
            ..env(NotifyProtocol::Osc9)
        };
        let Some(Delivery::System(argv)) =
            plan(&note("t\"; do shell script \"x", "b"), system, mac, 1)
        else {
            panic!("system delivery");
        };
        assert_eq!(argv[0], "osascript");
        assert_eq!(
            argv[7], "t\", do shell script \"x",
            "text stays an argv item, never script"
        );
    }

    #[test]
    fn detect_reads_the_terminal() {
        let pick = |vars: &[(&str, &str)]| {
            let vars: Vec<(String, String)> = vars
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect();
            HostEnv::from_env(|key| vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
        };
        assert_eq!(
            pick(&[("TERM", "xterm-kitty")]).auto_protocol,
            NotifyProtocol::Osc99
        );
        assert_eq!(
            pick(&[("TERM_PROGRAM", "ghostty")]).auto_protocol,
            NotifyProtocol::Osc777
        );
        assert_eq!(
            pick(&[("TERM_PROGRAM", "WezTerm")]).auto_protocol,
            NotifyProtocol::Osc777
        );
        assert_eq!(
            pick(&[("TERM", "foot")]).auto_protocol,
            NotifyProtocol::Osc777
        );
        assert_eq!(
            pick(&[("TERM_PROGRAM", "iTerm.app")]).auto_protocol,
            NotifyProtocol::Osc9
        );
        assert!(pick(&[("TMUX", "/tmp/tmux-501/default,1,0")]).tmux);
        assert!(!pick(&[]).tmux);
    }
}
