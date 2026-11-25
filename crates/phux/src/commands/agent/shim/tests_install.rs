//! Unit tests for install/uninstall and the wrapper's crash behavior.

use super::install::{install_claude_into, installed_shim_schema, uninstall_claude_from};
use super::rc::install_rc_block;
use super::wrapper::render_wrapper;
use super::{BLOCK_BEGIN, HOOKS, SHIM_SCHEMA};
use crate::commands::sh_quote;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

/// Run an interactive shim outside a pane and collect its real PTY output.
fn run_on_pty(wrapper: &Path, envs: &[(&str, &str)]) -> (u32, String) {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Read as _;

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open test pty");
    let mut command = CommandBuilder::new(wrapper);
    command.env("SHELL", "/bin/sh");
    command.env("TERM", "xterm-256color");
    for key in [
        "PHUX_TERMINAL_ID",
        "PHUX_AGENT_PHUX_BIN",
        "PHUX_CHAT_RECORD",
        "PHUX_AGENT_ADDRESS",
    ] {
        command.env_remove(key);
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = pair
        .slave
        .spawn_command(command)
        .expect("spawn wrapper under pty");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let drain = std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut buf = [0_u8; 8192];
        while let Ok(read) = reader.read(&mut buf) {
            if read == 0 {
                break;
            }
            output.extend_from_slice(&buf[..read]);
        }
        output
    });
    drop(pair.master);
    // A hung child is bounded by nextest's slow-timeout, as in the other PTY tests.
    let status = child.wait().expect("wait for the wrapper to exit");
    let output = drain.join().expect("pty reader thread");
    (
        status.exit_code(),
        String::from_utf8_lossy(&output).into_owned(),
    )
}

/// Install over a stale, unstamped (schema-1) install, then uninstall: the
/// stale shim is replaced and reported, the hook settings are private, and
/// uninstall removes exactly its own three files plus the rc block.
#[test]
fn install_over_a_stale_shim_upgrades_it_and_uninstall_removes_exactly_its_own_files() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let shim_dir = dir.path().join("shims");
    let rc = dir.path().join("rc");
    let phux = dir.path().join("phux");
    let real = dir.path().join("real-claude");
    std::fs::write(&real, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(installed_shim_schema(&shim_dir.join("claude")), None);

    // A stale install: the schema-1 wrapper, its hook settings, a v1
    // manifest (no `shim_schema` key), and the rc block.
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::write(
        shim_dir.join("claude"),
        "#!/bin/sh\nrun_phux agent set \"$target\" --name claude --state working\n",
    )
    .unwrap();
    std::fs::write(shim_dir.join("claude-hooks.json"), b"{}").unwrap();
    std::fs::write(
        shim_dir.join(super::MANIFEST),
        serde_json::json!({
            "schema_version": 1,
            "real_claude": real,
            "shell": "zsh",
            "rc": rc,
        })
        .to_string(),
    )
    .unwrap();
    // A file phux does not own, to prove uninstall does not over-reach.
    std::fs::write(shim_dir.join("keep-me"), b"not ours").unwrap();

    let user_rc = "# my rc\nalias ll='ls -l'\n";
    std::fs::write(&rc, user_rc).unwrap();
    install_rc_block(&rc, "export PATH=stale:\"$PATH\"").unwrap();

    // --- upgrade ------------------------------------------------------
    let report = install_claude_into(&shim_dir, &rc, "zsh", Some(&real), &phux).unwrap();
    assert_eq!(
        report.replaced,
        Some(1),
        "the stale shim must be recognized, not silently overwritten",
    );
    let installed = std::fs::read_to_string(shim_dir.join("claude")).unwrap();
    assert_eq!(
        installed_shim_schema(&shim_dir.join("claude")),
        Some(SHIM_SCHEMA)
    );
    assert!(
        !installed.contains("--state"),
        "the upgraded shim must not declare state:\n{installed}"
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(shim_dir.join(super::MANIFEST)).unwrap()).unwrap();
    assert_eq!(manifest["shim_schema"], serde_json::json!(SHIM_SCHEMA));

    // The hook settings register every `HOOKS` row and stay private.
    let settings = shim_dir.join("claude-hooks.json");
    let mode = std::fs::metadata(&settings).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "hook settings must stay private");
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    let hooks = json["hooks"].as_object().expect("hooks object");
    assert_eq!(hooks.len(), HOOKS.len());
    let shim = sh_quote(&shim_dir.join("claude").to_string_lossy());
    for (event, matcher, arm) in HOOKS {
        let entry = &hooks[*event][0];
        assert_eq!(entry["matcher"], *matcher, "{event}");
        assert_eq!(
            entry["hooks"][0]["command"],
            format!("{shim} --phux-hook {arm}"),
            "{event}"
        );
    }

    // Re-installing is idempotent and leaves exactly one managed block.
    let again = install_claude_into(&shim_dir, &rc, "zsh", Some(&real), &phux).unwrap();
    assert_eq!(again.replaced, Some(SHIM_SCHEMA), "already current");
    let rc_text = std::fs::read_to_string(&rc).unwrap();
    assert_eq!(rc_text.matches(BLOCK_BEGIN).count(), 1);
    assert!(
        rc_text.starts_with(user_rc),
        "user bytes survive: {rc_text:?}"
    );

    // --- uninstall ----------------------------------------------------
    let removed = uninstall_claude_from(&shim_dir).unwrap();
    assert_eq!(removed.as_deref(), Some(rc.as_path()));
    assert_eq!(
        std::fs::read_to_string(&rc).unwrap(),
        user_rc,
        "the rc returns byte-for-byte to what the user had",
    );
    let mut left: Vec<_> = std::fs::read_dir(&shim_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    left.sort();
    assert_eq!(
        left,
        vec![std::ffi::OsString::from("keep-me")],
        "uninstall removes its own three files and nothing else",
    );
    assert!(uninstall_claude_from(&shim_dir).unwrap().is_none());
}

/// A chat process already has its routing context. Adoption shims must not
/// wrap it in `phux new`, even on a real terminal outside a phux pane.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one linear PTY-driven install scenario"
)]
#[allow(
    clippy::literal_string_with_formatting_args,
    reason = "shell parameter expansion in fixture scripts"
)]
fn chat_launches_preserve_context_and_claude_settings_outside_a_pane() {
    use super::agents::install_agent_into;
    use crate::skill::setup::SkillAgent;

    let dir = tempfile::tempdir().expect("scratch dir");
    let shim_dir = dir.path().join("shims");
    let rc = dir.path().join("rc");
    let real = dir.path().join("real-agent");
    let phux = dir.path().join("fake-phux");
    let phux_log = dir.path().join("fake-phux.log");
    let injection = dir.path().join("must-not-exist");
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::write(
        &real,
        r#"#!/bin/sh
set -u
[ -t 0 ] && [ -t 1 ] || exit 71
[ -z "${PHUX_TERMINAL_ID:-}" ] || exit 72
printf 'record:%s\naddress:%s\n' "$PHUX_CHAT_RECORD" "$PHUX_AGENT_ADDRESS"
if [ "$FAKE_AGENT" = claude ]; then
  [ "$#" -eq 2 ] && [ "$1" = --settings ] || exit 73
  [ -s "$2" ] || exit 74
  printf 'settings:%s\n' "$2"
else
  [ "$#" -eq 0 ] || exit 75
fi
exit 17
"#,
    )
    .unwrap();
    std::fs::write(&phux, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FAKE_LOG\"\n").unwrap();
    for path in [&real, &phux] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    for agent in [
        SkillAgent::Claude,
        SkillAgent::Codex,
        SkillAgent::Opencode,
        SkillAgent::Pi,
    ] {
        let wrapper = shim_dir.join(agent.name());
        if agent == SkillAgent::Claude {
            install_claude_into(&shim_dir, &rc, "bash", Some(&real), &phux).unwrap();
        } else {
            std::fs::write(
                shim_dir.join(format!("{}-install.json", agent.name())),
                serde_json::json!({ "real": real, "rc": rc }).to_string(),
            )
            .unwrap();
            install_agent_into(agent, &shim_dir, &rc, "bash", &phux).unwrap();
        }
        // The shim neither parses nor executes the record. Even malformed
        // context reaches the CLI unchanged, where validation fails closed.
        for record in [
            "/a chat's record.json",
            "malformed; $(touch \"$SHIM_INJECTION\")\n'\"",
        ] {
            std::fs::write(&phux_log, b"").unwrap();
            let (status, output) = run_on_pty(
                &wrapper,
                &[
                    ("PHUX_CHAT_RECORD", record),
                    ("PHUX_AGENT_ADDRESS", "reviewer@chat-test"),
                    ("FAKE_AGENT", agent.name()),
                    ("FAKE_LOG", phux_log.to_str().unwrap()),
                    ("SHIM_INJECTION", injection.to_str().unwrap()),
                ],
            );
            let output = output.replace("\r\n", "\n");
            assert_eq!(status, 17, "{agent:?}: {output}");
            assert!(output.contains(&format!("record:{record}\n")), "{output}");
            assert!(output.contains("address:reviewer@chat-test\n"), "{output}");
            if agent == SkillAgent::Claude {
                let settings = shim_dir.join("claude-hooks.json");
                assert!(
                    output.contains(&format!("settings:{}\n", settings.display())),
                    "{output}"
                );
            }
            assert_eq!(std::fs::read_to_string(&phux_log).unwrap(), "", "{output}");
            assert!(!injection.exists(), "record text must never execute");
        }

        // An empty record is not an explicit chat launch: ordinary adoption
        // still goes through phux rather than silently disabling wrapping.
        let (status, output) = run_on_pty(
            &wrapper,
            &[
                ("PHUX_CHAT_RECORD", ""),
                ("FAKE_LOG", phux_log.to_str().unwrap()),
            ],
        );
        assert_eq!(status, 0, "{agent:?}: {output}");
        assert!(
            std::fs::read_to_string(&phux_log)
                .unwrap()
                .starts_with("new "),
            "{agent:?}: expected ordinary phux wrapping: {output}"
        );
        assert!(!output.contains("record:"), "{output}");
    }
}

/// A rendered wrapper, run for real on a pty (the launch path needs a
/// tty), must not relaunch Claude after the phux session started and
/// later died.
#[test]
#[allow(clippy::too_many_lines, reason = "one linear pty-driven scenario")]
#[allow(
    clippy::literal_string_with_formatting_args,
    reason = "`${...}` here is shell parameter expansion in a fixture script, not a std format arg"
)]
fn wrapper_never_relaunches_claude_after_a_mid_session_crash() {
    /// The `phux` subcommands the wrapper dispatched, one per line.
    fn phux_calls(phux_log: &Path) -> Vec<String> {
        std::fs::read_to_string(phux_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn write_script(path: &Path, body: &str) {
        std::fs::write(path, body).expect("write fixture script");
        let mut perms = std::fs::metadata(path)
            .expect("stat fixture script")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod fixture script");
    }

    let dir = tempfile::tempdir().expect("scratch dir");
    let real = dir.path().join("real-claude");
    let fake_phux = dir.path().join("fake-phux");
    let wrapper = dir.path().join("claude");
    let settings = dir.path().join("claude-hooks.json");
    let log = dir.path().join("real-claude.log");
    // Every `phux` dispatch: distinguishes launched-through-phux from
    // exec'd-directly, which Claude's invocation count cannot.
    let phux_log = dir.path().join("fake-phux.log");

    // Stands in for the real `claude`: records that it ran, then exits
    // with a caller-controlled status (simulating either a clean exit
    // or Claude itself dying mid-session).
    write_script(
        &real,
        &format!(
            "#!/bin/sh\nprintf 'invoked\\n' >> {log}\nexit \"${{FAKE_CLAUDE_EXIT:-0}}\"\n",
            log = sh_quote(&log.to_string_lossy()),
        ),
    );
    // Stands in for `phux new -c <cwd> -- <cmd...>`: either fails
    // outright before ever running the session command (simulating a
    // launch that never started), or execs the given command exactly
    // like the real subcommand would.
    write_script(
        &fake_phux,
        &format!(
            "#!/bin/sh\nset -u\nprintf '%s\\n' \"$1\" >> {phux_log}\nif [ \"${{FAKE_PHUX_LAUNCH_FAIL:-0}}\" = \"1\" ]; then\n  exit 3\nfi\nshift 4\nexec \"$@\"\n",
            phux_log = sh_quote(&phux_log.to_string_lossy()),
        ),
    );
    let rendered = render_wrapper(&real, &fake_phux, &wrapper, &settings).unwrap();
    write_script(&wrapper, &rendered);

    // Scenario A: the session runs and Claude exits cleanly.
    std::fs::write(&log, b"").unwrap();
    std::fs::write(&phux_log, b"").unwrap();
    let (status, output) = run_on_pty(&wrapper, &[("FAKE_CLAUDE_EXIT", "0")]);
    assert_eq!(status, 0, "clean session exit; output:\n{output}");
    assert_eq!(
        phux_calls(&phux_log),
        vec!["new".to_owned()],
        "the wrapper must launch Claude THROUGH a phux session, not exec \
             it directly; output:\n{output}"
    );
    assert_eq!(
        std::fs::read_to_string(&log).unwrap().lines().count(),
        1,
        "Claude must run exactly once; output:\n{output}"
    );
    assert!(!output.contains("abnormally"), "{output}");
    assert!(!output.contains("launch failed"), "{output}");

    // Scenario B: the session starts, then dies mid-run. The real exit
    // status must propagate and Claude must NOT be invoked again.
    std::fs::write(&log, b"").unwrap();
    std::fs::write(&phux_log, b"").unwrap();
    let (status, output) = run_on_pty(&wrapper, &[("FAKE_CLAUDE_EXIT", "17")]);
    assert_eq!(
        status, 17,
        "mid-session crash status must propagate; output:\n{output}"
    );
    assert_eq!(
        phux_calls(&phux_log),
        vec!["new".to_owned()],
        "the crash must be a crash of the phux-launched session; output:\n{output}"
    );
    assert!(output.contains("phux session ended abnormally"), "{output}");
    assert!(!output.contains("launch failed"), "{output}");
    assert_eq!(
        std::fs::read_to_string(&log).unwrap().lines().count(),
        1,
        "Claude must not be relaunched after the session already started; output:\n{output}"
    );

    // Scenario C: `phux new` fails before the session starts; falling
    // back to a direct Claude exactly once is correct.
    std::fs::write(&log, b"").unwrap();
    std::fs::write(&phux_log, b"").unwrap();
    let (status, output) = run_on_pty(
        &wrapper,
        &[("FAKE_PHUX_LAUNCH_FAIL", "1"), ("FAKE_CLAUDE_EXIT", "0")],
    );
    assert_eq!(
        status, 0,
        "pre-launch failure falls back to direct Claude; output:\n{output}"
    );
    assert_eq!(
        phux_calls(&phux_log),
        vec!["new".to_owned()],
        "the fallback must follow a launch that was actually ATTEMPTED, \
             and must not retry it; output:\n{output}"
    );
    assert!(output.contains("phux launch failed"), "{output}");
    assert!(!output.contains("abnormally"), "{output}");
    assert_eq!(
        std::fs::read_to_string(&log).unwrap().lines().count(),
        1,
        "the direct fallback must still run Claude exactly once; output:\n{output}"
    );
}
