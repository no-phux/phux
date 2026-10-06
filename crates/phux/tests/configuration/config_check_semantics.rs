//! End-to-end coverage for the semantic pass of `phux config check`
//! (phux-i0e8.3.2): a typo'd action name must exit 1 with a did-you-mean
//! suggestion on the human surface, and `--json` must carry the new
//! fault labels (`unknown name`, `bad chord`) for scripts and CI.

#![allow(clippy::expect_used, reason = "tests")]

use tempfile::TempDir;

/// Write `config` into a tempdir and run `phux config check` against its
/// explicit path, returning `(exit_code, stdout)`.
fn run_check(tmp: &TempDir, config: &str, json: bool) -> (i32, String) {
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, config).expect("write config under test");
    let mut args = vec!["config", "check"];
    let path_str = path.to_str().expect("utf-8 temp path");
    args.push(path_str);
    if json {
        args.push("--json");
    }
    let out = crate::common::phux_cmd(crate::runner::phux_bin())
        // Isolate from the developer's real config; check reads only the
        // explicit PATH, but never trust a test that depends on $HOME.
        .env("XDG_CONFIG_HOME", tmp.path())
        .args(&args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The umbrella trap end to end: `"kill-pain"` loads fine and binds a key
/// to nothing. The CLI must exit 1 and say what was probably meant.
#[test]
fn typoed_action_exits_one_with_a_suggestion() {
    let tmp = TempDir::new().expect("tempdir");
    let (code, stdout) = run_check(
        &tmp,
        "[keybindings.prefix-table]\nq = \"kill-pain\"\n",
        false,
    );
    assert_eq!(code, 1, "findings must exit 1; stdout:\n{stdout}");
    assert!(
        stdout.contains("keybindings.prefix-table.q"),
        "finding must name the binding:\n{stdout}"
    );
    assert!(
        stdout.contains("unknown name"),
        "finding must carry the fault label:\n{stdout}"
    );
    assert!(
        stdout.contains("did you mean `kill-pane`?"),
        "finding must carry the suggestion:\n{stdout}"
    );
}

/// `--json` carries the new fault labels so a dotfiles CI job can react
/// to semantic findings the same way it reacts to schema ones.
#[test]
fn json_output_carries_the_new_fault_labels() {
    let tmp = TempDir::new().expect("tempdir");
    let config = "[keybindings.prefix-table]\nq = \"kill-pain\"\n\"w-\" = \"detach\"\n";
    let (code, stdout) = run_check(&tmp, config, true);
    assert_eq!(code, 1, "findings must exit 1; stdout:\n{stdout}");

    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("check emits valid JSON");
    assert_eq!(doc["ok"], serde_json::json!(false));
    let faults: Vec<&str> = doc["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .map(|finding| finding["fault"].as_str().expect("fault is a string"))
        .collect();
    assert!(
        faults.contains(&"unknown name"),
        "missing `unknown name` in {faults:?}"
    );
    assert!(
        faults.contains(&"bad chord"),
        "missing `bad chord` in {faults:?}"
    );
}

/// A PATH the caller names must exist: a typo'd path in a dotfiles CI gate
/// cannot pass as "clean". Exit 2 (the check could not run), on both
/// surfaces.
#[test]
fn a_named_path_that_does_not_exist_cannot_pass() {
    let tmp = TempDir::new().expect("tempdir");
    let missing = tmp.path().join("no-such-config.toml");
    let missing = missing.to_str().expect("utf-8 temp path");
    for json in [false, true] {
        let mut args = vec!["config", "check", missing];
        if json {
            args.push("--json");
        }
        let out = crate::common::phux_cmd(crate::runner::phux_bin())
            .env("XDG_CONFIG_HOME", tmp.path())
            .args(&args)
            .output()
            .expect("run phux binary");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "json={json}; stderr:\n{stderr}");
        assert!(
            stderr.contains("no-such-config.toml"),
            "names the path; json={json}; stderr:\n{stderr}"
        );
    }
}
