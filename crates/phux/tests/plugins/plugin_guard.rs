//! Guards on `phux plugin install` / `update`: git is not handed options,
//! fetches time out, symlinks are refused, and recorded commits are pinned.
//! No test contacts the network.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

struct TestHome {
    _tmp: TempDir,
    xdg_config: PathBuf,
    xdg_data: PathBuf,
    scratch: PathBuf,
}

impl TestHome {
    fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let scratch = tmp.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        Self {
            xdg_config: tmp.path().join("config"),
            xdg_data: tmp.path().join("data"),
            scratch,
            _tmp: tmp,
        }
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        self.run_with(args, |_| {})
    }

    fn run_with(&self, args: &[&str], edit: impl FnOnce(&mut Command)) -> (i32, String, String) {
        let mut cmd = crate::common::phux_cmd(crate::runner::phux_bin());
        cmd.env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("XDG_DATA_HOME", &self.xdg_data)
            .current_dir(&self.scratch)
            .args(args);
        edit(&mut cmd);
        let out = cmd.output().expect("run phux");
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn plugins_dir(&self) -> PathBuf {
        self.xdg_data.join("phux").join("plugins")
    }
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "user.name=Test",
        ])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn write_plugin(dir: &Path, id: &str, version: &str, build: bool) {
    std::fs::create_dir_all(dir).unwrap();
    let build = if build {
        "\n[[build]]\ncommand = [\"sh\", \"-c\", \"echo built > build-out.txt\"]\n"
    } else {
        ""
    };
    std::fs::write(
        dir.join("phux-plugin.toml"),
        format!(
            "id = \"{id}\"\nname = \"Guard\"\nversion = \"{version}\"\n\
             min_phux_version = \"0.0.1\"\n{build}\n\
             [[actions]]\nid = \"open\"\ntitle = \"Open\"\ncommand = [\"true\"]\n"
        ),
    )
    .unwrap();
}

fn executable(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn head(repo: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn option_shaped_ref_never_reaches_git() {
    let home = TestHome::new();
    let marker = home.scratch.join("git-invoked");
    let fake = home.scratch.join("bin").join("git");
    executable(
        &fake,
        &format!("#!/bin/sh\necho invoked > {}\nexit 1\n", marker.display()),
    );
    let path = format!(
        "{}:{}",
        fake.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let (code, stdout, stderr) = home.run_with(
        &["plugin", "install", "--", "--upload-pack=evil://x"],
        |cmd| {
            cmd.env("PATH", &path);
        },
    );
    assert_ne!(code, 0);
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("starts with '-'"), "{stderr}");
    assert!(stderr.contains("rejected before git"), "{stderr}");
    assert!(!marker.exists(), "git was invoked for an option-shaped ref");

    let (code, _, stderr) = home.run_with(
        &[
            "plugin",
            "install",
            "file:///tmp/phux-empty-rev",
            "--rev",
            "",
        ],
        |cmd| {
            cmd.env("PATH", &path);
        },
    );
    assert_ne!(code, 0);
    assert!(stderr.contains("--rev must name"), "{stderr}");
    assert!(!marker.exists(), "empty --rev must not spawn git");
}

#[test]
fn git_fetch_timeout_kills_the_process() {
    let home = TestHome::new();
    let pidfile = home.scratch.join("git.pid");
    let fake = home.scratch.join("bin").join("git");
    executable(
        &fake,
        &format!("#!/bin/sh\necho $$ > {}\nsleep 60\n", pidfile.display()),
    );
    let path = format!(
        "{}:{}",
        fake.parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let (code, _, stderr) = home.run_with(
        &["plugin", "install", "file:///tmp/phux-timeout-probe"],
        |cmd| {
            cmd.env("PATH", &path);
            cmd.env("PHUX_PLUGIN_GIT_TIMEOUT_MS", "300");
        },
    );
    assert_ne!(code, 0, "{stderr}");
    assert!(stderr.contains("timed out"), "{stderr}");
    let pid = std::fs::read_to_string(&pidfile).expect("fake git recorded its pid");
    let alive = Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "git process {pid} still alive after timeout");
}

#[test]
fn cloned_symlink_is_refused() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    write_plugin(&repo, "example.symlink", "0.1.0", false);
    std::os::unix::fs::symlink("/etc/passwd", repo.join("leak")).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "link"]);
    let url = format!("file://{}", repo.display());

    let (code, stdout, stderr) = home.run(&["plugin", "install", &url]);
    assert_ne!(code, 0, "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("is a symlink"), "{stderr}");
    assert!(!home.plugins_dir().join("example.symlink").exists());
}

#[test]
fn remote_build_requires_yes_off_a_terminal() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    write_plugin(&repo, "example.remote-build", "0.1.0", true);
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "build"]);
    let url = format!("file://{}", repo.display());

    let (code, _, stderr) = home.run(&["plugin", "install", &url]);
    assert_ne!(code, 0, "{stderr}");
    assert!(stderr.contains("--yes"), "{stderr}");
    assert!(!home.plugins_dir().join("example.remote-build").exists());

    let (code, _, stderr) = home.run(&["plugin", "install", &url, "--yes"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        home.plugins_dir()
            .join("example.remote-build")
            .join("build-out.txt")
            .is_file()
    );
}

#[test]
fn rev_pins_a_commit_until_latest() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    write_plugin(&repo, "example.pin", "0.1.0", false);
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "v1"]);
    let pinned = head(&repo);
    write_plugin(&repo, "example.pin", "0.2.0", false);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "v2"]);
    let url = format!("file://{}", repo.display());

    let (code, stdout, stderr) = home.run(&["plugin", "install", &url, "--rev", &pinned, "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["installed"]["version"], "0.1.0");
    assert_eq!(value["installed"]["rev"], pinned);

    let short = &pinned[..7];
    let (code, _, stderr) = home.run(&["plugin", "install", &url, "--rev", short]);
    assert_ne!(code, 0, "short SHA fetch should say so; stderr={stderr}");
    assert!(stderr.contains("full 40-character SHA"), "{stderr}");

    let (code, stdout, stderr) = home.run(&["plugin", "update", "example.pin", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["updated"][0]["rev"], pinned);
    let installed = std::fs::read_to_string(
        home.plugins_dir()
            .join("example.pin")
            .join("phux-plugin.toml"),
    )
    .unwrap();
    assert!(installed.contains("version = \"0.1.0\""), "{installed}");

    let (code, stdout, stderr) =
        home.run(&["plugin", "update", "example.pin", "--latest", "--json"]);
    assert_eq!(code, 0, "{stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_ne!(value["updated"][0]["rev"], pinned);
    let installed = std::fs::read_to_string(
        home.plugins_dir()
            .join("example.pin")
            .join("phux-plugin.toml"),
    )
    .unwrap();
    assert!(installed.contains("version = \"0.2.0\""), "{installed}");
}

#[test]
fn update_refuses_a_rewritten_branch_without_latest() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    write_plugin(&repo, "example.ff", "0.1.0", false);
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "v1"]);
    let first = head(&repo);
    let url = format!("file://{}", repo.display());
    let (code, _, stderr) = home.run(&["plugin", "install", &url]);
    assert_eq!(code, 0, "{stderr}");

    write_plugin(&repo, "example.ff", "0.2.0", false);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "v2"]);
    let (code, stdout, stderr) = home.run(&["plugin", "update", "example.ff", "--json"]);
    assert_eq!(
        code, 0,
        "fast-forward update should succeed; stderr={stderr}"
    );
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let recorded = value["updated"][0]["rev"].as_str().unwrap().to_owned();
    assert_ne!(recorded, first);

    git(&repo, &["reset", "--quiet", "--hard", &first]);
    write_plugin(&repo, "example.ff", "0.3.0", false);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "rewrite"]);
    let (code, _, stderr) = home.run(&["plugin", "update", "example.ff"]);
    assert_ne!(code, 0, "{stderr}");
    assert!(stderr.contains("not a descendant"), "{stderr}");
    assert!(stderr.contains("--latest"), "{stderr}");
    let installed = std::fs::read_to_string(
        home.plugins_dir()
            .join("example.ff")
            .join("phux-plugin.toml"),
    )
    .unwrap();
    assert!(installed.contains("version = \"0.2.0\""), "{installed}");

    let (code, _, stderr) = home.run(&["plugin", "update", "example.ff", "--latest"]);
    assert_eq!(code, 0, "{stderr}");
    let installed = std::fs::read_to_string(
        home.plugins_dir()
            .join("example.ff")
            .join("phux-plugin.toml"),
    )
    .unwrap();
    assert!(installed.contains("version = \"0.3.0\""), "{installed}");
}

fn manifest_with_build(dir: &Path, id: &str, version: &str, script: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("phux-plugin.toml"),
        format!(
            "id = \"{id}\"\nname = \"Guard\"\nversion = \"{version}\"\n\
             min_phux_version = \"0.0.1\"\n\n\
             [[build]]\ncommand = [\"sh\", \"-c\", \"{script}\"]\n\n\
             [[actions]]\nid = \"open\"\ntitle = \"Open\"\ncommand = [\"true\"]\n"
        ),
    )
    .unwrap();
}

#[test]
fn update_changed_build_steps_need_yes_off_a_terminal() {
    let home = TestHome::new();
    let repo = home.scratch.join("repo");
    manifest_with_build(&repo, "evil", "0.1.0", "echo A > approved.txt");
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "step A"]);
    let url = format!("file://{}", repo.display());

    let (code, _, stderr) = home.run_with(&["plugin", "install", &url, "--yes"], |cmd| {
        cmd.stdin(std::process::Stdio::null());
    });
    assert_eq!(code, 0, "{stderr}");
    let installed = home.plugins_dir().join("evil");
    assert!(installed.join("approved.txt").is_file());

    manifest_with_build(&repo, "evil", "0.2.0", "echo B > slipped.txt");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "step B"]);
    let (code, _, stderr) = home.run_with(&["plugin", "update", "evil"], |cmd| {
        cmd.stdin(std::process::Stdio::null());
    });
    assert_ne!(code, 0, "{stderr}");
    assert!(stderr.contains("--yes"), "{stderr}");
    assert!(stderr.contains("Nothing was installed"), "{stderr}");
    assert!(installed.join("approved.txt").is_file());
    assert!(
        !installed.join("slipped.txt").exists(),
        "changed build step B must not run"
    );

    let (code, _, stderr) = home.run_with(&["plugin", "update", "evil", "--yes"], |cmd| {
        cmd.stdin(std::process::Stdio::null());
    });
    assert_eq!(code, 0, "{stderr}");
    assert!(installed.join("slipped.txt").is_file());

    manifest_with_build(&repo, "evil", "0.2.1", "echo B > slipped.txt");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "same steps"]);
    let (code, _, stderr) = home.run_with(&["plugin", "update", "evil"], |cmd| {
        cmd.stdin(std::process::Stdio::null());
    });
    assert_eq!(
        code, 0,
        "unchanged build steps should not ask again; stderr={stderr}"
    );
}

#[test]
fn git_ssh_command_is_kept_and_gains_batch_mode() {
    let home = TestHome::new();
    let log = home.scratch.join("ssh.log");
    let wrapper = home.scratch.join("ssh-wrap");
    executable(
        &wrapper,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$0\" \"$@\" > {}\nexit 1\n",
            log.display()
        ),
    );
    let (code, _, stderr) = home.run_with(
        &["plugin", "install", "ssh://git@127.0.0.1/no-such-plugin"],
        |cmd| {
            cmd.env(
                "GIT_SSH_COMMAND",
                format!("{} --custom-key", wrapper.display()),
            );
            cmd.stdin(std::process::Stdio::null());
        },
    );
    assert_ne!(code, 0, "{stderr}");
    let recorded = std::fs::read_to_string(&log).expect("ssh wrapper was invoked");
    assert!(
        recorded.contains(wrapper.to_str().unwrap()),
        "wrapper was replaced: {recorded}"
    );
    assert!(recorded.contains("--custom-key"), "{recorded}");
    assert!(recorded.contains("BatchMode=yes"), "{recorded}");
}

#[test]
fn plugin_new_rejects_dot() {
    let home = TestHome::new();
    let (code, _, stderr) = home.run(&["plugin", "new", "."]);
    assert_ne!(code, 0, "{stderr}");
    assert!(stderr.contains("not a plugin id"), "{stderr}");
    assert!(!home.scratch.join("phux-plugin.toml").exists());
}
