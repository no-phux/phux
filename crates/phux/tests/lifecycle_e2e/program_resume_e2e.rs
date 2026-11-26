//! Default config leaves OSC 7577 off. A correctly signed report creates no
//! record, restore does not run it, and the pane comes back as its shell.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Output;
use std::time::Duration;

use crate::common;
use crate::hermetic;

const BUDGET: Duration = Duration::from_secs(30);

fn start(dir: &Path) -> hermetic::Server {
    let server = hermetic::start_server(
        dir,
        &["--no-seed"],
        &[("HOME", dir.to_str().expect("UTF-8"))],
    );
    assert!(common::wait_until_accepting(&dir.join("s.sock")));
    server
}

fn phux(dir: &Path, args: &[&str]) -> Output {
    let socket = dir.join("s.sock");
    hermetic::await_success(dir, args, &[("PHUX_SOCKET", socket.as_path())])
}

fn screen_text(dir: &Path) -> String {
    let out = phux(dir, &["snapshot", "work", "--scrollback", "--json"]);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
#[ignore = "restarts a real server; e2e lane"]
fn program_resume_off_ignores_a_signed_report_and_the_pane_comes_back() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().canonicalize().expect("canonical");
    hermetic::prepare_dirs(&dir);
    let marker = dir.join("ran");
    let env_file = dir.join("token-env");
    let shell = dir.join("pane.sh");
    std::fs::write(
        &shell,
        format!(
            r#"#!/bin/sh
marker={marker:?}
envfile={env:?}
if [ -e "$marker" ]; then
  echo CAME-BACK
  exec sleep 600
fi
touch "$marker"
if [ -n "${{OSC7577_TOKEN+x}}" ]; then
  printf 'set\n' > "$envfile"
else
  printf 'unset\n' > "$envfile"
fi
token=${{OSC7577_TOKEN:-dummy-token-replace-me}}
now=$(date +%s)
b64() {{ printf '%s' "$1" | base64 | tr -d '\n'; }}
body=$(printf 'pid=%s:time=%s:argv=%s,%s' "$$" "$now" "$(b64 echo)" "$(b64 RESUMED-7577)")
mac=$(printf '%s:%s' "$body" "$token" | {{ sha256sum 2>/dev/null || shasum -a 256; }} | cut -c1-64)
printf '\033]7577;%s:mac=%s\033\\' "$body" "$mac"
echo SENT-7577
exec sleep 600
"#,
            marker = marker.display(),
            env = env_file.display(),
        ),
    )
    .expect("pane script");
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    // Default `[restore]`: program-resume stays off. Only the shell is set,
    // so a relaunch is this script again and not a raw archived command.
    std::fs::write(
        dir.join("config/phux/config.toml"),
        format!("[defaults]\nshell = {:?}\n", shell.display()),
    )
    .expect("config");

    let mut server = start(&dir);
    let created = phux(&dir, &["new", "--json", "-s", "work"]);
    assert!(
        created.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    common::until(BUDGET, || {
        screen_text(&dir).contains("SENT-7577").then_some(())
    })
    .expect("the signed report was written");
    assert_eq!(
        std::fs::read_to_string(&env_file)
            .expect("token env")
            .trim(),
        "unset",
        "program-resume off must not set OSC7577_TOKEN"
    );
    server.sigkill();
    drop(server);

    let mut server = start(&dir);
    let dry = phux(&dir, &["restore", "--dry-run"]);
    let dry_text = String::from_utf8_lossy(&dry.stdout);
    assert!(
        !dry_text.contains("RESUMED-7577") && !dry_text.contains("resume"),
        "a signed report must not become a record: {dry_text}"
    );
    let restored = phux(&dir, &["restore", "--json"]);
    assert!(
        restored.status.success(),
        "restore failed: {} {}",
        String::from_utf8_lossy(&restored.stdout),
        String::from_utf8_lossy(&restored.stderr)
    );
    let screen = common::until(BUDGET, || {
        let text = screen_text(&dir);
        text.contains("CAME-BACK").then_some(text)
    })
    .expect("the shell came back");
    assert!(
        !screen.contains("RESUMED-7577"),
        "the signed command must not run: {screen}"
    );
    server.sigkill();
}
