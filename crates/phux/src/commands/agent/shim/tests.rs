//! Unit tests for the shim: the test harness, the rc block, and the
//! wrapper's shape.

use super::rc::{shell_activation, without_managed_block};
use super::wrapper::render_wrapper;
use super::{BLOCK_BEGIN, BLOCK_END, HOOKS};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

/// The wrapper with fixed placeholder paths, for the text assertions.
pub(super) fn rendered() -> String {
    render_wrapper(
        Path::new("/real/claude"),
        Path::new("/bin/phux"),
        Path::new("/data/phux/shims/claude"),
        Path::new("/data/phux/shims/claude-hooks.json"),
    )
    .unwrap()
}

/// A fake `phux` that logs every argv line to `$FAKE_LOG`, stands in for
/// `agent hook-payload` with the canned `$FAKE_FIELDS` line (the real
/// helper is pinned by `hook_payload.rs`), and logs whatever an
/// `emit --data -` fed it on stdin — so a test can see exactly which
/// bytes of a hook payload ever reached a `phux` process.
pub(super) const FAKE_PHUX: &str = concat!(
    "#!/bin/sh\n",
    "printf '%s\\n' \"$*\" >> \"$FAKE_LOG\"\n",
    "case \"$1 ${2:-}\" in\n",
    "  \"agent hook-payload\") cat > /dev/null; printf '%s\\n' \"$FAKE_FIELDS\"; exit 0 ;;\n",
    "  \"agent hook-transcript\") cat > /dev/null; [ -z \"${FAKE_TRANSCRIPT:-}\" ] || printf '%s\\n' \"$FAKE_TRANSCRIPT\"; exit 0 ;;\n",
    "esac\n",
    "case \"$*\" in *\"--data -\") printf 'stdin:%s\\n' \"$(cat)\" >> \"$FAKE_LOG\" ;; esac\n",
    "exit 0\n",
);

/// A rendered wrapper plus the fake `phux` it dispatches to, on disk.
pub(super) struct Harness {
    pub(super) _dir: tempfile::TempDir,
    pub(super) wrapper: std::path::PathBuf,
    pub(super) log: std::path::PathBuf,
}

impl Harness {
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().expect("scratch dir");
        let fake_phux = dir.path().join("fake-phux");
        let wrapper = dir.path().join("claude");
        let settings = dir.path().join("claude-hooks.json");
        std::fs::write(&fake_phux, FAKE_PHUX).unwrap();
        std::fs::set_permissions(&fake_phux, std::fs::Permissions::from_mode(0o755)).unwrap();
        let text =
            render_wrapper(Path::new("/real/claude"), &fake_phux, &wrapper, &settings).unwrap();
        std::fs::write(&wrapper, text).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            _dir: dir,
            wrapper,
            log: dir_log(&fake_phux),
        }
    }

    /// Run `<wrapper> --phux-hook <arm>` with `payload` on stdin and the
    /// helper answering `fields`, and return every `phux` argv line the
    /// fake logged, in order.
    pub(super) fn hook(&self, arm: &str, payload: &str, fields: &str, raw: bool) -> Vec<String> {
        self.hook_env(arm, payload, fields, raw, &[])
    }

    /// [`Self::hook`] with extra environment: `FAKE_TRANSCRIPT` is the
    /// line the fake `agent hook-transcript` prints, and
    /// `PHUX_AGENT_TRANSCRIPT` the opt-out. Both are unset otherwise.
    pub(super) fn hook_env(
        &self,
        arm: &str,
        payload: &str,
        fields: &str,
        raw: bool,
        env: &[(&str, &str)],
    ) -> Vec<String> {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let _ = std::fs::remove_file(&self.log);
        let mut cmd = Command::new(&self.wrapper);
        cmd.args(["--phux-hook", arm])
            .env_remove("PHUX_AGENT_PHUX_BIN")
            .env("PHUX_TERMINAL_ID", "42")
            .env("FAKE_LOG", &self.log)
            .env("FAKE_FIELDS", fields)
            .env_remove("FAKE_TRANSCRIPT")
            .env_remove("PHUX_AGENT_TRANSCRIPT")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if raw {
            cmd.env("PHUX_AGENT_EMIT_RAW", "1");
        } else {
            cmd.env_remove("PHUX_AGENT_EMIT_RAW");
        }
        let mut child = cmd.spawn().expect("spawn the wrapper");
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(payload.as_bytes())
            .unwrap();
        let out = child.wait_with_output().expect("wrapper exit");
        assert!(
            out.status.success(),
            "hook {arm} exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stdout.is_empty(),
            "a hook must print nothing to Claude: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

pub(super) fn dir_log(fake_phux: &Path) -> std::path::PathBuf {
    fake_phux.with_file_name("fake-phux.log")
}

#[test]
fn rc_block_removal_preserves_every_user_owned_byte() {
    let input = format!("before\n{BLOCK_BEGIN}\nexport PATH=x\n{BLOCK_END}\nafter\n");
    assert_eq!(without_managed_block(&input).unwrap(), "before\nafter\n");
    assert_eq!(without_managed_block("untouched\n").unwrap(), "untouched\n");
    assert!(without_managed_block(BLOCK_BEGIN).is_err());
}

#[test]
fn shell_activation_quotes_paths_and_uses_native_fish_syntax() {
    let path = Path::new("/tmp/a path/it's-here");
    let quoted = "'/tmp/a path/it'\\''s-here'";
    assert_eq!(
        shell_activation("zsh", path).unwrap(),
        format!("export PATH={quoted}:\"$PATH\"")
    );
    assert_eq!(
        shell_activation("fish", path).unwrap(),
        format!("fish_add_path --prepend {quoted}")
    );
}

/// Every hook the installer registers has an arm in the wrapper's stream
/// path; identity, ask, and clear sit outside it.
#[test]
fn every_registered_hook_arm_is_handled_and_vice_versa() {
    let wrapper = rendered();
    let stream = section(&wrapper, "stream_state() {", "set_state() {");
    for (event, _, arm) in HOOKS {
        assert!(
            stream.contains(&format!("{arm})")),
            "{event} is registered as `--phux-hook {arm}` but stream_state has no `{arm})` arm",
        );
    }
    assert!(!stream.contains("agent set") && !stream.contains("agent clear"));
    assert!(!stream.contains("run_phux ask"));
}

/// The text of `wrapper` between the first occurrence of `from` and
/// the next occurrence of `to`.
pub(super) fn section<'a>(wrapper: &'a str, from: &str, to: &str) -> &'a str {
    let start = wrapper
        .find(from)
        .unwrap_or_else(|| panic!("no `{from}` in wrapper"));
    let end = wrapper[start..]
        .find(to)
        .unwrap_or_else(|| panic!("no `{to}` after `{from}`"));
    &wrapper[start..start + end]
}

/// The launch path's exit trap runs `clear` at most once, however the
/// wrapper leaves: INT/TERM/HUP re-enter through EXIT, and the stream's
/// `session_end` is terminal.
#[test]
fn the_exit_trap_is_idempotent() {
    let wrapper = rendered();
    let trap = section(&wrapper, "  ended=false", "  trap 'cleanup' EXIT");
    assert!(
        trap.contains("[ \"$ended\" = false ] || return 0"),
        "{trap}"
    );
    assert!(trap.contains("ended=true"), "{trap}");
    assert!(wrapper.contains("trap 'exit 130' INT"));
    assert!(wrapper.contains("trap 'exit 143' TERM"));
    assert!(wrapper.contains("trap 'exit 129' HUP"));
}

/// The wrapper parses under `sh -n` (and, where a `shellcheck` is on
/// PATH, lints clean at `-s sh`) — it is installed as `/bin/sh` and has
/// to stay POSIX.
#[test]
fn the_wrapper_is_posix_sh() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let wrapper = dir.path().join("claude");
    std::fs::write(&wrapper, rendered()).unwrap();
    let parsed = std::process::Command::new("sh")
        .arg("-n")
        .arg(&wrapper)
        .output()
        .expect("run sh -n");
    assert!(
        parsed.status.success(),
        "sh -n: {}",
        String::from_utf8_lossy(&parsed.stderr)
    );
    if let Ok(lint) = std::process::Command::new("shellcheck")
        .args(["-s", "sh"])
        .arg(&wrapper)
        .output()
    {
        assert!(
            lint.status.success(),
            "shellcheck -s sh:\n{}",
            String::from_utf8_lossy(&lint.stdout)
        );
    }
}
