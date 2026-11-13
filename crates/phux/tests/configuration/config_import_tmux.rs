//! Exercise the actual CLI with a deterministic read-only tmux executable.
//! The live tmux smoke is separate so CI needs no external multiplexer.

#![cfg(unix)]
#![allow(clippy::expect_used, reason = "test harness")]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Output;

use tempfile::TempDir;

fn fixture() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = r#"#!/bin/sh
if [ "$1" = -S ]; then
  [ "$2" = '/tmp/import source.sock' ] || exit 42
  shift 2
fi
case "$*" in
  'show-options -g')
    printf '%s\n' 'prefix C-b' 'prefix2 None' 'mouse on' 'history-limit 4000' "default-shell '/bin/sh'" 'base-index 0';;
  'show-options -gw') printf '%s\n' 'mode-keys vi';;
  'list-keys')
    printf '%s\n' 'bind-key -T prefix v split-window -h' 'bind-key -T prefix x confirm-before -p "kill?" kill-window -t :2' 'bind-key -T prefix r run-shell "touch /tmp/should-never-execute"';;
  *) exit 43;;
esac
"#;
    std::fs::write(dir.path().join("tmux"), script).expect("script");
    std::fs::set_permissions(
        dir.path().join("tmux"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("chmod");
    dir
}

fn run(dir: &Path, args: &[&str]) -> Output {
    crate::common::phux_cmd(crate::runner::phux_bin())
        .env("PATH", dir)
        .env_remove("TMUX")
        .env("XDG_CONFIG_HOME", dir.join("xdg"))
        .args(args)
        .output()
        .expect("phux")
}

#[test]
fn import_tmux_preview_apply_validate_and_refuse_overwrite() {
    let dir = fixture();
    let out = run(
        dir.path(),
        &[
            "config",
            "import-tmux",
            "--tmux-socket",
            "/tmp/import source.sock",
            "--json",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
    let preview: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(preview["applied"], false);
    assert!(preview["saved"].is_null());
    assert!(!dir.path().join("xdg").exists());
    let entries = preview["entries"].as_array().expect("entries");
    assert!(entries.iter().any(|e| {
        e["source"]
            .as_str()
            .is_some_and(|s| s.contains("confirm-before"))
            && e["status"] == "unsupported"
    }));

    let out = run(dir.path(), &["config", "import-tmux", "--apply", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let installed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(installed["toml"], preview["toml"]);
    assert_eq!(installed["applied"], true);
    let config = dir.path().join("xdg/phux/config.toml");
    let original = std::fs::read_to_string(&config).expect("config");
    let out = run(dir.path(), &["config", "check", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run(dir.path(), &["config", "keys", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let keys: serde_json::Value = serde_json::from_slice(&out.stdout).expect("keys");
    assert!(keys.to_string().contains("tmux-import.toml"));
    let out = run(dir.path(), &["config", "import-tmux", "--apply", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&out.stderr).expect("error JSON");
    assert_eq!(error["error"]["code"], "invalid_config");
    assert_eq!(std::fs::read_to_string(config).expect("config"), original);
}

#[test]
fn import_tmux_output_is_exclusive_and_preview_is_native_toml() {
    let dir = fixture();
    let out = run(dir.path(), &["config", "import-tmux"]);
    assert!(out.status.success());
    let body = String::from_utf8(out.stdout).expect("TOML");
    assert!(
        phux_config::check::check(&body, &dir.path().join("draft.toml"))
            .expect("check")
            .is_ok()
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("unsupported"));
    let output = dir.path().join("my import.toml");
    let args = [
        "config",
        "import-tmux",
        "--output",
        output.to_str().expect("path"),
    ];
    assert!(run(dir.path(), &args).status.success());
    assert_eq!(std::fs::read_to_string(&output).expect("output"), body);
    assert_eq!(run(dir.path(), &args).status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(output).expect("output"), body);
    assert!(
        !run(
            dir.path(),
            &["config", "import-tmux", "--apply", "--output", "unused"]
        )
        .status
        .success()
    );
    assert!(
        !run(dir.path(), &["config", "import-tmux", "--socket", "unused"])
            .status
            .success()
    );
}

#[test]
fn import_tmux_missing_tool_has_structured_error_and_no_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = run(dir.path(), &["config", "import-tmux", "--apply", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&out.stderr).expect("error JSON");
    assert!(
        error["error"]["message"]
            .as_str()
            .expect("message")
            .contains("cannot run tmux")
    );
    assert!(!dir.path().join("xdg").exists());
}

#[test]
fn import_tmux_preserves_clipboard_denial_and_functional_display_commands() {
    let dir = fixture();
    std::fs::write(
        dir.path().join("tmux"),
        r#"#!/bin/sh
case "$*" in
  'show-options -g') printf '%s\n' 'prefix C-b' 'set-clipboard off';;
  'show-options -gw') printf '\n';;
  'list-keys')
    printf '%s\n' \
      'bind-key -T prefix r source-file ~/.tmux.conf \; display-message -I -t %1' \
      'bind-key -T prefix R source-file ~/.tmux.conf \; display-message "reloaded"';;
  *) exit 43;;
esac
"#,
    )
    .expect("script");
    let out = run(dir.path(), &["config", "import-tmux", "--json"]);
    assert!(out.status.success(), "{out:?}");
    let preview: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let table: toml::Table =
        toml::from_str(preview["toml"].as_str().expect("TOML")).expect("parse");
    assert_eq!(table["defaults"]["clipboard-write"].as_str(), Some("deny"));
    let prefix = &table["keybindings"]["prefix-table"];
    assert_eq!(prefix["r"].as_str(), Some("none"));
    assert_eq!(prefix["R"].as_str(), Some("reload-config"));
    assert!(!dir.path().join("xdg").exists());
}

#[test]
fn migrate_print_preserves_overrides_privacy_and_functional_commands() {
    let dir = tempfile::tempdir().expect("home");
    std::fs::write(
        dir.path().join(".tmux.conf"),
        r#"bind '"' new-window
unbind '"'
bind - split-window -v
set -g set-clipboard off
set -sg escape-time 500
bind r source-file ~/.tmux.conf \; display-message -I -t %1
bind R source-file ~/.tmux.conf \; display-message "reloaded"
"#,
    )
    .expect("config");
    let out = crate::common::phux_cmd(crate::runner::phux_bin())
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("xdg"))
        .args(["migrate", "--print", "--json", "--source", "tmux"])
        .output()
        .expect("phux");
    assert!(out.status.success(), "{out:?}");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    let prompt = doc["prompt"].as_str().expect("prompt");
    assert!(prompt.contains("bind '\"' new-window`\n  nothing to do:"));
    assert!(prompt.contains("clipboard-write = \"deny\""));
    assert!(prompt.contains("escape-time 500 is not migrated"));
    assert!(prompt.contains("display-message -I -t %1`\n  not written:"));
    assert!(prompt.contains("\"R\" = \"reload-config\""));
    assert!(doc["launched"].is_null());
    assert!(!dir.path().join("xdg").exists());
}

#[test]
fn import_tmux_bounds_output_and_inspection_time_before_writing() {
    let dir = fixture();
    for (script, expected) in [
        (
            "#!/bin/sh\nexec /usr/bin/head -c 4194305 /dev/zero\n",
            "exceeded 4 MiB",
        ),
        (
            "#!/bin/sh\nexec /bin/sleep 30\n",
            "timed out after 5 seconds",
        ),
    ] {
        std::fs::write(dir.path().join("tmux"), script).expect("script");
        let out = run(dir.path(), &["config", "import-tmux", "--apply", "--json"]);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&out.stderr).expect("error JSON");
        assert!(
            error["error"]["message"]
                .as_str()
                .expect("message")
                .contains(expected)
        );
        assert!(!dir.path().join("xdg").exists());
    }
}
