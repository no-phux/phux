//! Turning paired mode on when `phux pair` finds no remote listener bound:
//! the one question, the config write and restart, and each refusal. Only an
//! unset `[policy] mode` with a private network address to bind may change
//! anything.

use std::io::{BufRead, Write as _};
use std::path::Path;

use phux_protocol::wire::ListenerDisabledReason;

use super::{
    ListenerQueryError, LiveListeners, NO_BOUND_LISTENER, query_live_listeners, rebind_server,
};

/// The words of the refusal when paired mode would bind nothing because
/// this computer has no private network address. Rune recognizes them to
/// show its Tailscale guidance, so they are a contract with that caller.
const NO_PRIVATE_ADDRESS: &str = "no private network address";

/// The one question a terminal gets when nothing is bound because
/// `[policy] mode` is unset. Default is no.
pub(super) const PAIRED_MODE_PROMPT: &str = "phones need paired mode: the server will listen on your tailnet for enrolled devices only. turn it on and restart the server? (panes are kept) [y/N] ";

/// Where the paired-mode question is asked. Production reads the terminal;
/// tests inject the answer and record whether a prompt happened.
pub(super) trait ModePrompt {
    /// Whether stdin is a terminal, so a question has someone to answer it.
    fn is_interactive(&self) -> bool;
    /// Show [`PAIRED_MODE_PROMPT`]. Called only when [`Self::is_interactive`].
    fn prompt(&mut self);
    /// One line from the person, without a trailing requirement. `None` at EOF.
    fn read_answer(&mut self) -> Option<String>;
}

/// The terminal [`ModePrompt`].
pub(super) struct StdinModePrompt;

impl ModePrompt for StdinModePrompt {
    fn is_interactive(&self) -> bool {
        std::io::IsTerminal::is_terminal(&std::io::stdin())
    }

    fn prompt(&mut self) {
        eprint!("phux: {PAIRED_MODE_PROMPT}");
        let _ = std::io::stderr().flush();
    }

    fn read_answer(&mut self) -> Option<String> {
        let stdin = std::io::stdin();
        let mut line = String::new();
        let read = stdin.lock().read_line(&mut line).ok()?;
        (read > 0).then_some(line)
    }
}

/// The config and the running server `phux pair` changes when turning
/// paired mode on. Tests substitute both.
pub(super) trait ListenerSetup {
    /// The listeners bound right now.
    fn query(&mut self) -> Result<LiveListeners, ListenerQueryError>;
    /// `[policy] mode` in the user's config. `Ok(None)` is unset.
    fn policy_mode(&mut self) -> Result<Option<phux_config::PolicyMode>, String>;
    /// Whether this computer has an overlay address paired mode would bind.
    fn has_private_address(&mut self) -> bool;
    /// Write `[policy] mode = "paired"`, preserving the rest of the file.
    fn write_paired(&mut self) -> Result<(), String>;
    /// Restart the server in place so the new mode binds a listener.
    fn rebind(&mut self) -> Result<(), String>;
}

/// The real config file and the server on `socket`.
pub(super) struct LiveListenerSetup<'a> {
    /// Server to ask and to restart.
    socket: &'a Path,
}

impl ListenerSetup for LiveListenerSetup<'_> {
    fn query(&mut self) -> Result<LiveListeners, ListenerQueryError> {
        query_live_listeners(self.socket)
    }

    fn policy_mode(&mut self) -> Result<Option<phux_config::PolicyMode>, String> {
        let path = phux_config::loader::config_path();
        phux_config::loader::load_from(&path)
            .map(|config| config.policy.mode)
            .map_err(|err| format!("{}: {err}", path.display()))
    }

    fn has_private_address(&mut self) -> bool {
        !phux_config::overlay::detect().is_empty()
    }

    fn write_paired(&mut self) -> Result<(), String> {
        let path = phux_config::loader::config_path();
        crate::commands::service::write_paired_mode(&path)?;
        info!(
            "pair: set [policy] mode = \"paired\" in {}; restarting the server (panes are kept)",
            path.display()
        );
        Ok(())
    }

    fn rebind(&mut self) -> Result<(), String> {
        rebind_server(self.socket).map_err(|err| {
            format!(
                "{err}; [policy] mode = \"paired\" is already in config.toml, so `phux upgrade` \
                 binds a listener with panes intact"
            )
        })
    }
}

/// `y` or `yes`, the same answers the other confirmation prompts take.
pub(super) fn is_affirmative(reply: &str) -> bool {
    matches!(reply.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Whether to write paired mode: `--enable`, or a yes at the one prompt.
/// `--json` is machine output, so it never asks, the same as a run with
/// no terminal. `--enable` still writes.
pub(super) fn accept_paired_mode(enable: bool, json: bool, prompt: &mut dyn ModePrompt) -> bool {
    if enable {
        return true;
    }
    if json || !prompt.is_interactive() {
        return false;
    }
    prompt.prompt();
    prompt
        .read_answer()
        .is_some_and(|line| is_affirmative(&line))
}

/// One line when nothing is bound because `[policy] mode` is unset.
pub(super) fn mode_unset_refusal(socket: &Path) -> String {
    format!(
        "the server at {} {NO_BOUND_LISTENER} because [policy] mode is unset, so none was \
         minted; run `phux pair --enable` or set `[policy] mode = \"paired\"` (`phux doctor`)",
        socket.display()
    )
}

/// One line when paired mode would bind nothing: no overlay address.
pub(super) fn no_private_address_refusal(socket: &Path) -> String {
    format!(
        "the server at {} {NO_BOUND_LISTENER}, and this computer has {NO_PRIVATE_ADDRESS} \
         (no tailnet) for paired mode to bind, so [policy] mode was left unset and none was \
         minted; sign in to Tailscale and rerun `phux pair`",
        socket.display()
    )
}

/// One line when a mode is set and something else left the listener unbound.
pub(super) fn other_unbound_refusal(socket: &Path, disabled: &[String]) -> String {
    let why = if disabled.is_empty() {
        String::new()
    } else {
        format!(" ({})", disabled.join("; "))
    };
    format!(
        "the server at {} {NO_BOUND_LISTENER}{why}, so none was minted; open one, panes kept: \
         `phux service install --adopt --quic 0.0.0.0:8788`",
        socket.display()
    )
}

/// [`ensure_remote_listener`] on the terminal and the real server, printing
/// the refusal.
pub(super) fn bound_remote_listener(
    socket: &Path,
    enable: bool,
    json: bool,
) -> Option<LiveListeners> {
    ensure_remote_listener(
        socket,
        enable,
        json,
        &mut StdinModePrompt,
        &mut LiveListenerSetup { socket },
    )
    .inspect_err(|refusal| error!("pair: {refusal}"))
    .ok()
}

/// The listeners a mint may use. A bound listener is returned as the server
/// reported it. An unset `[policy] mode` is the one case that offers to
/// write `paired` and restart, and only when an overlay address exists for
/// it to bind; any other unbound cause is a refusal.
pub(super) fn ensure_remote_listener(
    socket: &Path,
    enable: bool,
    json: bool,
    prompt: &mut dyn ModePrompt,
    setup: &mut dyn ListenerSetup,
) -> Result<LiveListeners, String> {
    let disabled = match setup.query() {
        Ok(live) => return Ok(live),
        Err(ListenerQueryError::Unavailable(message)) => return Err(message),
        Err(ListenerQueryError::Unbound { disabled }) => disabled,
    };
    match setup.policy_mode() {
        Err(err) => {
            return Err(format!(
                "the server at {} {NO_BOUND_LISTENER}, and [policy] mode could not be read ({err})",
                socket.display()
            ));
        }
        Ok(Some(_)) => return Err(other_unbound_refusal(socket, &disabled)),
        Ok(None) => {}
    }
    // Paired mode binds overlay addresses, and any `--quic`/`--listen` the
    // unset mode refused. With neither, the write and restart would change
    // the config and still leave nothing bound.
    let refused = ListenerDisabledReason::PolicyModeRefused.as_str();
    let explicit = disabled.iter().any(|row| row.contains(refused));
    if !explicit && !setup.has_private_address() {
        return Err(no_private_address_refusal(socket));
    }
    if !accept_paired_mode(enable, json, prompt) {
        return Err(mode_unset_refusal(socket));
    }
    setup.write_paired()?;
    setup.rebind()?;
    bound_or_refusal(socket, setup.query())
}

/// A listener query whose unbound answer is the mode-agnostic refusal.
pub(super) fn bound_or_refusal(
    socket: &Path,
    queried: Result<LiveListeners, ListenerQueryError>,
) -> Result<LiveListeners, String> {
    queried.map_err(|err| match err {
        ListenerQueryError::Unavailable(message) => message,
        ListenerQueryError::Unbound { disabled } => other_unbound_refusal(socket, &disabled),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(wss: Option<&str>, quic: Option<&str>) -> LiveListeners {
        LiveListeners {
            wss: wss.map(str::to_owned),
            quic: quic.map(str::to_owned),
        }
    }

    /// The prompt is one question, defaulting to no. `y` and `yes` accept;
    /// anything else, including an empty line, declines.
    #[test]
    fn the_paired_mode_prompt_defaults_to_no() {
        assert!(!PAIRED_MODE_PROMPT.contains('\n'));
        assert!(PAIRED_MODE_PROMPT.ends_with("[y/N] "));
        assert!(is_affirmative("y"));
        assert!(is_affirmative(" YES "));
        assert!(!is_affirmative("n"));
        assert!(!is_affirmative(""));
        assert!(!is_affirmative("yep"));

        let mut prompt = ScriptedPrompt::terminal(Some("y\n"));
        assert!(accept_paired_mode(false, false, &mut prompt));
        assert_eq!((prompt.prompted, prompt.reads), (1, 1));

        prompt.answer = Some("\n".to_owned());
        assert!(!accept_paired_mode(false, false, &mut prompt));
        prompt.answer = Some("n\n".to_owned());
        assert!(!accept_paired_mode(false, false, &mut prompt));
        prompt.answer = None;
        assert!(!accept_paired_mode(false, false, &mut prompt));

        prompt.interactive = false;
        prompt.answer = Some("y\n".to_owned());
        assert!(!accept_paired_mode(false, false, &mut prompt));
        assert_eq!(
            (prompt.prompted, prompt.reads),
            (4, 4),
            "a non-interactive run must not ask"
        );

        assert!(accept_paired_mode(true, false, &mut prompt));
        assert_eq!(
            (prompt.prompted, prompt.reads),
            (4, 4),
            "--enable must not ask"
        );
    }

    /// A listener that is already bound skips the prompt, the config, and
    /// the restart.
    #[test]
    fn a_bound_listener_is_left_alone() {
        let mut prompt = ScriptedPrompt::silent();
        let mut setup = ScriptedSetup::unbound(None);
        setup.queries.clear();
        setup
            .queries
            .push_back(Ok(live(Some("100.64.0.2:8787"), Some("100.64.0.2:8788"))));
        let socket = Path::new("/run/phux.sock");
        let found =
            ensure_remote_listener(socket, true, false, &mut prompt, &mut setup).expect("bound");
        assert_eq!(
            found,
            live(Some("100.64.0.2:8787"), Some("100.64.0.2:8788"))
        );
        assert_eq!(prompt.prompted, 0);
        assert!(!setup.wrote && !setup.rebound);
        assert_eq!(setup.mode_reads, 0);
    }

    /// Answering yes writes paired mode and continues with the listener the
    /// restart bound. No, and an empty answer, leave the config alone.
    #[test]
    fn an_unset_mode_on_a_terminal_writes_paired_mode_only_on_yes() {
        let socket = Path::new("/run/phux.sock");
        for answer in [Some("n\n"), Some("\n"), None] {
            let mut prompt = ScriptedPrompt::terminal(answer);
            let mut setup = ScriptedSetup::unbound(None);
            let err = ensure_remote_listener(socket, false, false, &mut prompt, &mut setup)
                .expect_err("declined");
            assert_eq!(err, mode_unset_refusal(socket));
            assert!(!err.contains('\n'), "{err}");
            assert_eq!(prompt.prompted, 1);
            assert!(!setup.wrote && !setup.rebound);
            assert_eq!(setup.queries.len(), 0, "a decline does not ask again");
        }

        let mut prompt = ScriptedPrompt::terminal(Some("y\n"));
        let mut setup = ScriptedSetup::unbound(None);
        setup
            .queries
            .push_back(Ok(live(Some("100.64.0.2:8787"), None)));
        let found = ensure_remote_listener(socket, false, false, &mut prompt, &mut setup)
            .expect("accepted");
        assert_eq!(found, live(Some("100.64.0.2:8787"), None));
        assert!(setup.wrote && setup.rebound);
        assert_eq!(prompt.prompted, 1);
    }

    /// `--json` on a terminal is the non-interactive path: no question, the
    /// config stays put, and the refusal names `--enable`. `--enable --json`
    /// still writes paired mode and continues.
    #[test]
    fn json_on_a_terminal_does_not_ask_and_leaves_the_config() {
        let socket = Path::new("/run/phux.sock");
        let mut prompt = ScriptedPrompt::terminal(Some("y\n"));
        let mut setup = ScriptedSetup::unbound(None);
        let err = ensure_remote_listener(socket, false, true, &mut prompt, &mut setup)
            .expect_err("json does not ask");
        assert_eq!(err, mode_unset_refusal(socket));
        assert!(!err.contains('\n'), "{err}");
        assert!(
            err.contains("phux pair --enable") && err.contains("[policy] mode = \"paired\""),
            "{err}"
        );
        assert_eq!((prompt.prompted, prompt.reads), (0, 0));
        assert!(!setup.wrote && !setup.rebound);

        setup.queries.push_back(Err(ListenerQueryError::Unbound {
            disabled: Vec::new(),
        }));
        setup
            .queries
            .push_back(Ok(live(Some("100.64.0.2:8787"), None)));
        let found = ensure_remote_listener(socket, true, true, &mut prompt, &mut setup)
            .expect("enable --json");
        assert_eq!(found, live(Some("100.64.0.2:8787"), None));
        assert_eq!((prompt.prompted, prompt.reads), (0, 0));
        assert!(setup.wrote && setup.rebound);
    }

    /// With no overlay address, paired mode would bind nothing: `--enable`
    /// and a terminal both get a refusal naming it, with no prompt, no
    /// config write and no restart.
    #[test]
    fn no_private_address_leaves_the_mode_unset_and_does_not_restart() {
        let socket = Path::new("/run/phux.sock");
        for enable in [true, false] {
            let mut prompt = ScriptedPrompt::terminal(Some("y\n"));
            let mut setup = ScriptedSetup::unbound(None);
            setup.private = false;
            let err = ensure_remote_listener(socket, enable, false, &mut prompt, &mut setup)
                .expect_err("nothing to bind");
            assert_eq!(err, no_private_address_refusal(socket));
            assert!(err.contains(NO_PRIVATE_ADDRESS) && err.contains(NO_BOUND_LISTENER));
            assert!(!err.contains('\n'), "{err}");
            assert_eq!(prompt.prompted, 0);
            assert!(!setup.wrote && !setup.rebound);
        }
    }

    /// A `--quic` the unset mode refused is something paired mode can bind,
    /// so a missing overlay address does not block `--enable`.
    #[test]
    fn a_refused_explicit_listener_binds_without_a_private_address() {
        let socket = Path::new("/run/phux.sock");
        let mut prompt = ScriptedPrompt::silent();
        let mut setup = ScriptedSetup::unbound(None);
        setup.private = false;
        setup.queries = std::collections::VecDeque::from([
            Err(ListenerQueryError::Unbound {
                disabled: vec!["quic at 127.0.0.1:4433 disabled (policy_mode_refused)".into()],
            }),
            Ok(live(None, Some("127.0.0.1:4433"))),
        ]);
        ensure_remote_listener(socket, true, false, &mut prompt, &mut setup).expect("bound");
        assert!(setup.wrote && setup.rebound);
    }

    /// A mode that is already set, with nothing bound, is a different cause:
    /// no prompt, no write, and the refusal names the command that binds one.
    #[test]
    fn a_set_mode_with_nothing_bound_names_its_remedy_and_does_not_ask() {
        let socket = Path::new("/run/phux.sock");
        for mode in [
            phux_config::PolicyMode::Paired,
            phux_config::PolicyMode::Local,
        ] {
            let mut prompt = ScriptedPrompt::terminal(Some("y\n"));
            let mut setup = ScriptedSetup::unbound(Some(mode));
            setup.queries.clear();
            setup.queries.push_back(Err(ListenerQueryError::Unbound {
                disabled: vec!["quic at ? disabled (no_overlay_address)".to_owned()],
            }));
            let err = ensure_remote_listener(socket, true, false, &mut prompt, &mut setup)
                .expect_err("mode is set");
            assert!(!err.contains('\n'), "{err}");
            assert!(err.contains("service install --adopt --quic"), "{err}");
            assert!(err.contains("no_overlay_address"), "{err}");
            assert!(!err.contains("phux pair --enable"), "{err}");
            assert_eq!(prompt.prompted, 0);
            assert!(!setup.wrote && !setup.rebound);
        }
    }

    /// A scripted yes/no. `prompt` records the question and does not print.
    struct ScriptedPrompt {
        interactive: bool,
        answer: Option<String>,
        prompted: usize,
        reads: usize,
    }

    impl ScriptedPrompt {
        fn silent() -> Self {
            Self {
                interactive: false,
                ..Self::terminal(None)
            }
        }

        fn terminal(answer: Option<&str>) -> Self {
            Self {
                interactive: true,
                answer: answer.map(str::to_owned),
                prompted: 0,
                reads: 0,
            }
        }
    }

    impl ModePrompt for ScriptedPrompt {
        fn is_interactive(&self) -> bool {
            self.interactive
        }

        fn prompt(&mut self) {
            self.prompted += 1;
        }

        fn read_answer(&mut self) -> Option<String> {
            self.reads += 1;
            self.answer.clone()
        }
    }

    /// A scripted listener report, policy mode, and the two side effects.
    struct ScriptedSetup {
        queries: std::collections::VecDeque<Result<LiveListeners, ListenerQueryError>>,
        mode: Option<phux_config::PolicyMode>,
        mode_reads: usize,
        private: bool,
        wrote: bool,
        rebound: bool,
    }

    impl ScriptedSetup {
        fn unbound(mode: Option<phux_config::PolicyMode>) -> Self {
            let mut queries = std::collections::VecDeque::new();
            queries.push_back(Err(ListenerQueryError::Unbound {
                disabled: Vec::new(),
            }));
            Self {
                queries,
                mode,
                mode_reads: 0,
                private: true,
                wrote: false,
                rebound: false,
            }
        }
    }

    impl ListenerSetup for ScriptedSetup {
        fn query(&mut self) -> Result<LiveListeners, ListenerQueryError> {
            self.queries.pop_front().expect("a scripted listener query")
        }

        fn policy_mode(&mut self) -> Result<Option<phux_config::PolicyMode>, String> {
            self.mode_reads += 1;
            Ok(self.mode)
        }

        fn has_private_address(&mut self) -> bool {
            self.private
        }

        fn write_paired(&mut self) -> Result<(), String> {
            self.wrote = true;
            Ok(())
        }

        fn rebind(&mut self) -> Result<(), String> {
            self.rebound = true;
            Ok(())
        }
    }
}
