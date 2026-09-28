//! Service-managed panes get a login shell (so profile PATH entries such as
//! Homebrew and Nix resolve); ordinary panes do not. The switch is the
//! `PHUX_SERVICE_MANAGED` marker `phux service install` stamps into the unit,
//! never a heuristic on environment shape.
//!
//! Runs the real binary with a real PTY and `/bin/sh` sourcing a fixture
//! `~/.profile`; only launchd/systemd is simulated, by `env_clear()` plus a
//! minimal `PATH` and the marker set directly.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::io::Write as _;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// The marker `phux service install` writes into the generated unit (a
/// literal: this suite drives the binary as a black box).
const SERVICE_MANAGED_ENV: &str = "PHUX_SERVICE_MANAGED";

/// launchd's own default `PATH` for an agent with no `EnvironmentVariables`
/// override (`man launchd.plist`), used here to build a realistically
/// minimal — not artificially empty — service environment.
const LAUNCHD_DEFAULT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Hang detector for the seed pane to write its result file.
const RESULT_DEADLINE: Duration = Duration::from_secs(20);

/// Poll cadence for every wait loop in this file.
const POLL: Duration = Duration::from_millis(50);

/// A running `phux server` child plus its private socket.
struct ServerGuard(common::ServerGuard);

impl std::ops::Deref for ServerGuard {
    type Target = common::ServerGuard;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ServerGuard {
    /// Start `phux server` from a cleared environment: only `HOME` (the fixture)
    /// and launchd's default `PATH`, so nothing from the test's own (often
    /// `nix develop`) shell leaks in. `service_managed` stamps
    /// [`SERVICE_MANAGED_ENV`] exactly as `phux service install` would.
    fn start(home: &Path, seed_command: &str, service_managed: bool) -> Self {
        let mut spawn = common::ServerGuard::builder("login")
            .session("svc")
            .seed_command(seed_command)
            .env_clear()
            .env("HOME", home)
            .env("PATH", LAUNCHD_DEFAULT_PATH);
        if service_managed {
            spawn = spawn.env(SERVICE_MANAGED_ENV, "1");
        }
        Self(spawn.start())
    }
}

/// A fixture `$HOME` whose `~/.profile` prepends `$HOME/bin` (holding an
/// executable marker) to `PATH`, as Homebrew/Nix profile snippets do. Only a
/// login shell sources it.
fn write_profile_fixture() -> TempDir {
    let home = TempDir::new().expect("tempdir");
    let bin = home.path().join("bin");
    std::fs::create_dir_all(&bin).expect("mkdir bin");

    let marker = bin.join("phux-profile-marker");
    let mut f = std::fs::File::create(&marker).expect("create marker");
    writeln!(f, "#!/bin/sh\necho PHUX_PROFILE_MARKER_FOUND").expect("write marker");
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o755))
            .expect("chmod marker");
    }

    let profile = home.path().join(".profile");
    std::fs::write(&profile, "PATH=\"$HOME/bin:$PATH\"\nexport PATH\n").expect("write .profile");

    home
}

/// Last line the seed shell writes, and the only safe signal that the record
/// is complete. See [`read_result_file`].
const RESULT_TERMINATOR: &str = "PHUX_RESULT_END";

/// The seed command: report `PATH`, try the profile-provided command, then
/// print a terminator (which still lands when the marker is not found).
fn seed_command(result_path: &Path) -> String {
    format!(
        "{{ printf '%s\\n' \"$PATH\"; phux-profile-marker; printf '%s\\n' \
         '{RESULT_TERMINATOR}'; }} >'{}' 2>&1",
        result_path.display()
    )
}

/// Poll `path` until the record is complete (its terminator line), or panic
/// at `deadline`: the writes share one truncating redirection but land at
/// different times, so a non-empty read can be a prefix.
fn read_result_file(path: &Path, deadline: Duration) -> String {
    let end = Instant::now() + deadline;
    let mut last = String::new();
    while Instant::now() < end {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if contents.contains(RESULT_TERMINATOR) {
                return contents;
            }
            last = contents;
        }
        std::thread::sleep(POLL);
    }
    panic!(
        "seed pane never wrote a complete result to {} within {deadline:?} \
         (waiting for the {RESULT_TERMINATOR} line); got so far: {last:?}",
        path.display()
    );
}

/// A service-managed server's pane gets the `PATH` a login `/bin/sh` produces
/// on this host, not a plain one's. Compared against the host's own login
/// shell because a CI image's `/etc/profile` can redirect `$HOME` before the
/// fixture is read; where the fixture is reachable, the marker must resolve
/// too (see [`fixture_profile_is_reachable`]).
#[test]
fn service_managed_pane_resolves_a_profile_provided_command() {
    let home = write_profile_fixture();
    let result_path = home.path().join("result");
    let seed = seed_command(&result_path);

    let _server = ServerGuard::start(home.path(), &seed, true);

    let contents = read_result_file(&result_path, RESULT_DEADLINE);
    let pane_path = contents.lines().next().unwrap_or_default().to_owned();

    let login = probe_shell_path(home.path(), true);
    let plain = probe_shell_path(home.path(), false);
    let diagnostics = diagnostics(home.path(), &login, &plain);

    assert_ne!(
        pane_path, plain,
        "a service-managed server's seed pane must NOT get a plain shell's \
         PATH — login-shell treatment was not applied.\n{diagnostics}"
    );
    assert_eq!(
        pane_path, login,
        "a service-managed server's seed pane must get the same PATH a login \
         `/bin/sh` produces on this host.\n{diagnostics}"
    );

    // Where the host lets the fixture's `~/.profile` through, hold the
    // stronger line too: the profile-provided command must actually resolve.
    if fixture_profile_is_reachable(home.path(), &login) {
        assert!(
            contents.contains("PHUX_PROFILE_MARKER_FOUND"),
            "the host's login shell does source the fixture `~/.profile` (its \
             bin directory is on the login PATH), so the profile-provided \
             command must resolve in the pane; got: {contents:?}\n{diagnostics}"
        );
    }
}

/// Whether this host's login shell reached the fixture's `~/.profile`.
fn fixture_profile_is_reachable(home: &Path, login_probe: &str) -> bool {
    login_probe.contains(&home.join("bin").display().to_string())
}

/// The host facts that decide who owns a failure here, rendered once.
fn diagnostics(home: &Path, login: &str, plain: &str) -> String {
    format!(
        "\nHost diagnostics:\n\
         \x20 fixture HOME      : {home_dir}\n\
         \x20 ~/.profile exists : {profile_exists}\n\
         \x20 marker executable : {marker_exec}\n\
         \x20 /bin/sh resolves  : {sh_target}\n\
         \x20 sh -l -c $HOME    : {login_home}\n\
         \x20 sh -l -c $PATH    : {login}\n\
         \x20 sh -c    $PATH    : {plain}\n\
         \x20 /etc/profile.d    : {profile_d}\n\
         \n\
         If the two probes above are identical, this host's `/bin/sh` gives a \
         login shell nothing extra and the comparison cannot detect the flag \
         at all. If `sh -l -c` reports a HOME other than the fixture, the \
         host's /etc/profile is resetting it and the fixture is unreachable \
         by construction.",
        home_dir = home.display(),
        profile_exists = home.join(".profile").exists(),
        marker_exec = home.join("bin/phux-profile-marker").exists(),
        sh_target = std::fs::read_link("/bin/sh").map_or_else(
            |_| "(not a symlink)".to_owned(),
            |p| p.display().to_string()
        ),
        login_home = probe_shell(home, true, "$HOME"),
        profile_d = probe_system_profile(),
    )
}

/// Run `/bin/sh [-l] -c expr` under the server's exact environment and
/// return the expansion: the host reference the pane is compared against.
fn probe_shell(home: &Path, login: bool, expr: &str) -> String {
    let mut cmd = Command::new("/bin/sh");
    cmd.env_clear();
    cmd.env("HOME", home);
    cmd.env("PATH", LAUNCHD_DEFAULT_PATH);
    if login {
        cmd.arg("-l");
    }
    cmd.arg("-c").arg(format!("printf %s \"{expr}\""));
    cmd.output().map_or_else(
        |e| format!("(probe failed: {e})"),
        |out| String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// The `PATH` a `[-l]` `/bin/sh` produces against the fixture home.
fn probe_shell_path(home: &Path, login: bool) -> String {
    probe_shell(home, login, "$PATH")
}

/// What the host's system profile does to a login shell, listed so a failure
/// names the file responsible instead of leaving it to be guessed.
fn probe_system_profile() -> String {
    let mut entries: Vec<String> = std::fs::read_dir("/etc/profile.d").map_or_else(
        |_| Vec::new(),
        |dir| {
            dir.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        },
    );
    entries.sort();
    if entries.is_empty() {
        "(none)".to_owned()
    } else {
        entries.join(" ")
    }
}

/// A server without the marker keeps plain shells: the profile command must
/// not resolve.
#[test]
fn ordinary_pane_does_not_source_the_profile_twice() {
    let home = write_profile_fixture();
    let result_path = home.path().join("result");
    let seed = seed_command(&result_path);

    let _server = ServerGuard::start(home.path(), &seed, false);

    let contents = read_result_file(&result_path, RESULT_DEADLINE);
    let pane_path = contents.lines().next().unwrap_or_default().to_owned();

    let login = probe_shell_path(home.path(), true);
    let plain = probe_shell_path(home.path(), false);
    let diagnostics = diagnostics(home.path(), &login, &plain);

    assert_eq!(
        pane_path, plain,
        "an ordinary (non-service) server's seed pane must get a plain \
         shell's PATH — login-shell treatment must stay conditional on the \
         service marker.\n{diagnostics}"
    );
    assert!(
        !contents.contains("PHUX_PROFILE_MARKER_FOUND"),
        "an ordinary (non-service) server's seed pane must NOT get \
         login-shell treatment — `~/.profile` must stay unsourced; \
         got: {contents:?}\n{diagnostics}"
    );
}
