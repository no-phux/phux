//! Login-shell PATH for the CLI Desktop installs at `~/.local/bin`.
//!
//! The profile path and the exported line match `scripts/installer/cli.sh`
//! `add_to_profile` and `shell_quote` in `scripts/installer/common.sh`, so a
//! line the shell installer already wrote is not appended again. Nothing here
//! writes a profile unless the caller asks.

use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;

/// Comment the shell installer writes above its PATH line.
pub(crate) const INSTALLER_MARKER: &str = "# Added by the phux installer";

const PROBE_BEGIN: &str = "PHUX_PATH_PROBE_BEGIN";
const PROBE_END: &str = "PHUX_PATH_PROBE_END";
const PROBE_STDOUT_CAP: usize = 64 * 1024;
const KEY_ZDOTDIR: &str = "ZDOTDIR=";
const KEY_XDG: &str = "XDG_CONFIG_HOME=";
const KEY_MODIFY: &str = "PHUX_NO_MODIFY_PATH=";

/// What a login-shell probe means for the CLI Desktop is using.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathStatus {
    /// `command -v phux` resolved to this CLI.
    OnPath,
    /// The shell ran and did not resolve this CLI. The user may add it.
    Missing,
    /// The probe timed out or failed. Show the command; do not write.
    Unverified,
}

/// Profile file and the exact line the shell installer would append for `~/.local/bin`.
pub(crate) fn profile_for(
    shell: &str,
    home: &Path,
    zdotdir: Option<&Path>,
    xdg_config: Option<&Path>,
    os: &str,
) -> (PathBuf, String) {
    let quoted = shell_quote(&home.join(".local/bin").to_string_lossy());
    let export = format!("export PATH={quoted}:\"$PATH\"");
    let name = Path::new(shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sh");
    match name {
        "fish" => {
            let base = xdg_config.unwrap_or(&home.join(".config")).to_path_buf();
            (
                base.join("fish/conf.d/phux.fish"),
                format!("fish_add_path -g {quoted}"),
            )
        }
        "zsh" => {
            let base = zdotdir.unwrap_or(home);
            (base.join(".zshrc"), export)
        }
        // macOS terminals start login shells, which read .bash_profile.
        "bash" if os == "Darwin" => (home.join(".bash_profile"), export),
        "bash" => (home.join(".bashrc"), export),
        _ => (home.join(".profile"), export),
    }
}

/// POSIX stand-in for `printf %q`: bare when no shell treats a character
/// specially, otherwise single-quoted. Same character class as the installer.
pub(crate) fn shell_quote(value: &str) -> String {
    let bare = !value.is_empty()
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '.' | '/' | ':' | '@' | '%' | '+' | '=' | '-')
        });
    if bare {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Environment a login shell actually has. A Finder or Dock launch does not
/// inherit `ZDOTDIR` or `PHUX_NO_MODIFY_PATH` from that shell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LoginEnv {
    pub zdotdir: Option<PathBuf>,
    pub xdg_config: Option<PathBuf>,
    /// Raw `PHUX_NO_MODIFY_PATH`. `None` when the shell left it unset or empty.
    pub modify_path: Option<std::ffi::OsString>,
}

/// `command -v phux` plus the shell variables printed between the sentinels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Probe {
    pub command: Option<PathBuf>,
    pub env: LoginEnv,
}

/// `Ok` when both sentinels were found. A marker matches with `ends_with`, so
/// rc noise before it does not hide the probe. `Err` when a sentinel is
/// missing, so a timeout fragment is not "not found".
pub(crate) fn parse_probe(stdout: &str) -> Result<Probe, ()> {
    let mut start = None;
    let mut end = None;
    let mut pos = 0;
    for line in stdout.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']).trim();
        if trimmed.ends_with(PROBE_BEGIN) {
            start = Some(pos + line.len());
            end = None;
        } else if trimmed.ends_with(PROBE_END) && start.is_some() {
            end = Some(pos);
        }
        pos += line.len();
    }
    let (Some(start), Some(end)) = (start, end) else {
        return Err(());
    };
    let mut probe = Probe {
        command: None,
        env: LoginEnv::default(),
    };
    for line in stdout[start..end].lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix(KEY_ZDOTDIR) {
            probe.env.zdotdir = nonempty_path(value);
        } else if let Some(value) = line.strip_prefix(KEY_XDG) {
            probe.env.xdg_config = nonempty_path(value);
        } else if let Some(value) = line.strip_prefix(KEY_MODIFY) {
            probe.env.modify_path = nonempty_os(value);
        } else if probe.command.is_none() {
            probe.command = Some(PathBuf::from(line));
        }
    }
    Ok(probe)
}

fn nonempty_path(value: &str) -> Option<PathBuf> {
    (!value.is_empty()).then(|| PathBuf::from(value))
}

fn nonempty_os(value: &str) -> Option<std::ffi::OsString> {
    (!value.is_empty()).then(|| std::ffi::OsString::from(value))
}

/// Whether `found` and `cli` are the same file after resolving symlinks.
pub(crate) fn resolves_to(found: &Path, cli: &Path) -> bool {
    match (found.canonicalize(), cli.canonicalize()) {
        (Ok(found), Ok(cli)) => found == cli,
        _ => false,
    }
}

pub(crate) fn classify(result: anyhow::Result<Option<PathBuf>>, cli: &Path) -> PathStatus {
    match result {
        Ok(Some(found)) if resolves_to(&found, cli) => PathStatus::OnPath,
        Ok(_) => PathStatus::Missing,
        Err(_) => PathStatus::Unverified,
    }
}

/// True when `path` is the per-user CLI at `~/.local/bin/phux`.
pub(crate) fn is_user_local_cli(home: &Path, path: &Path) -> bool {
    let expected = home.join(".local/bin/phux");
    path == expected || resolves_to(path, &expected)
}

/// Append `line` once, under `marker`, without truncating or rewriting.
/// `Ok(true)` when this call appended it. `Ok(false)` when the exact line was
/// already present. Creates a missing file or parent directory only under
/// `$HOME`. Refuses a symlink that leaves `$HOME` and a file owned by someone
/// else. `PHUX_NO_MODIFY_PATH` (anything other than empty or `0`) refuses
/// before touching the file.
pub(crate) fn append_once(profile: &Path, marker: &str, line: &str) -> anyhow::Result<bool> {
    append_guarded(
        profile,
        marker,
        line,
        changes_allowed(std::env::var_os("PHUX_NO_MODIFY_PATH").as_deref()),
    )
}

/// Same write as [`append_once`], with the login shell's allow/refuse flag.
/// `allow` comes from the probe when it succeeded, and from this process only
/// when the probe failed.
pub(crate) fn append_guarded(
    profile: &Path,
    marker: &str,
    line: &str,
    allow: bool,
) -> anyhow::Result<bool> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    append_once_in(Path::new(&home), profile, marker, line, allow)
}

/// `PHUX_NO_MODIFY_PATH` refuses the write unless it is unset, empty, or `0`.
pub(crate) fn changes_allowed(modify_path: Option<&std::ffi::OsStr>) -> bool {
    match modify_path {
        None => true,
        Some(value) => value.is_empty() || value == "0",
    }
}

/// `home` is the directory writes must stay inside. Tests pass a temp home
/// because this crate denies `unsafe` and cannot set `HOME` for the process.
fn append_once_in(
    home: &Path,
    profile: &Path,
    marker: &str,
    line: &str,
    allow: bool,
) -> anyhow::Result<bool> {
    if !allow {
        anyhow::bail!("PHUX_NO_MODIFY_PATH is set");
    }
    if !home.is_absolute() {
        anyhow::bail!("HOME is not absolute");
    }
    confine(home, profile)?;
    if profile.exists() {
        ensure_owned(profile)?;
    }
    if line_present(profile, line)? {
        return Ok(false);
    }
    ensure_parent(home, profile)?;
    confine(home, profile)?;
    if profile.exists() {
        ensure_owned(profile)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(profile)
        .with_context(|| format!("Could not update {}", profile.display()))?;
    let payload = format!("\n{marker}\n{line}\n");
    file.write_all(payload.as_bytes())
        .with_context(|| format!("Could not update {}", profile.display()))?;
    Ok(true)
}

fn line_present(profile: &Path, line: &str) -> anyhow::Result<bool> {
    if !profile.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(profile)
        .with_context(|| format!("Could not read {}", profile.display()))?;
    Ok(text.lines().any(|existing| existing == line))
}

fn confine(home: &Path, profile: &Path) -> anyhow::Result<()> {
    // macOS temp homes live under `/var`, which is a symlink to `/private/var`.
    // Compare the resolved profile with the resolved home, and allow that ancestor.
    let home = resolved_home(home);
    if !profile.is_absolute() {
        anyhow::bail!(
            "Refusing to change {}: it is outside {}",
            profile.display(),
            home.display()
        );
    }
    let mut current = PathBuf::new();
    current.push(Component::RootDir);
    for component in profile.components() {
        match component {
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => {
                anyhow::bail!(
                    "Refusing to change {}: the path leaves its directory",
                    profile.display()
                );
            }
            Component::Normal(part) => {
                current.push(part);
                if let Ok(meta) = std::fs::symlink_metadata(&current)
                    && meta.file_type().is_symlink()
                {
                    let target = std::fs::canonicalize(&current).with_context(|| {
                        format!(
                            "Refusing to change {}: cannot resolve symlink {}",
                            profile.display(),
                            current.display()
                        )
                    })?;
                    let target = lexical(&target);
                    if !symlink_stays_with_home(&home, &target) {
                        anyhow::bail!(
                            "Refusing to change {}: {} is a symlink outside {}",
                            profile.display(),
                            current.display(),
                            home.display()
                        );
                    }
                    current = target;
                }
            }
            Component::Prefix(_) => {
                anyhow::bail!("Refusing to change {}", profile.display());
            }
        }
    }
    let current = lexical(&current);
    if !current.starts_with(&home) || current == home {
        anyhow::bail!(
            "Refusing to change {}: it is outside {}",
            profile.display(),
            home.display()
        );
    }
    Ok(())
}

/// `home` with symlinks resolved. A missing home still resolves the existing prefix.
fn resolved_home(home: &Path) -> PathBuf {
    let home = lexical(home);
    if let Ok(path) = std::fs::canonicalize(&home) {
        return lexical(&path);
    }
    let mut resolved = PathBuf::new();
    let mut missing = false;
    for component in home.components() {
        if missing {
            resolved.push(component);
            continue;
        }
        let next = resolved.join(component);
        if next.exists() {
            resolved = std::fs::canonicalize(&next).map_or(next, |path| lexical(&path));
        } else {
            missing = true;
            resolved.push(component);
        }
    }
    resolved
}

/// An intermediate symlink may be inside home or an ancestor of it.
fn symlink_stays_with_home(home: &Path, target: &Path) -> bool {
    target.starts_with(home) || home.starts_with(target)
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn ensure_parent(home: &Path, profile: &Path) -> anyhow::Result<()> {
    let Some(parent) = profile.parent() else {
        anyhow::bail!("No directory for {}", profile.display());
    };
    if parent.exists() {
        if !parent.is_dir() {
            anyhow::bail!("{} is not a directory", parent.display());
        }
        return Ok(());
    }
    let home = lexical(home);
    let mut missing = Vec::new();
    let mut cursor = parent.to_path_buf();
    while !cursor.exists() {
        if !lexical(&cursor).starts_with(&home) {
            anyhow::bail!(
                "Refusing to create {} outside {}",
                cursor.display(),
                home.display()
            );
        }
        missing.push(cursor.clone());
        let Some(next) = cursor.parent() else {
            break;
        };
        if next == cursor {
            break;
        }
        cursor = next.to_path_buf();
    }
    for dir in missing.into_iter().rev() {
        std::fs::create_dir(&dir).with_context(|| format!("Could not create {}", dir.display()))?;
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_owned(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = path.metadata()?;
    if meta.uid() != nix::unistd::geteuid().as_raw() {
        anyhow::bail!(
            "Refusing to change {}: it is not owned by you",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owned(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// Run `"$SHELL" -lic` with sentinels around `command -v phux` and the shell's
/// `ZDOTDIR`, `XDG_CONFIG_HOME`, and `PHUX_NO_MODIFY_PATH`. Timeout and any
/// spawn or exit error are `Err`. Off the UI thread: this blocks until the
/// shell exits or the timeout kills its session.
pub(crate) fn probe_login_shell(shell: &Path, timeout: Duration) -> anyhow::Result<Probe> {
    let script = format!(
        "printf '%s\\n' '{PROBE_BEGIN}'; printf '{KEY_ZDOTDIR}%s\\n' \"${{ZDOTDIR-}}\"; \
         printf '{KEY_XDG}%s\\n' \"${{XDG_CONFIG_HOME-}}\"; \
         printf '{KEY_MODIFY}%s\\n' \"${{PHUX_NO_MODIFY_PATH-}}\"; \
         command -v phux || true; printf '%s\\n' '{PROBE_END}'"
    );
    let mut command = std::process::Command::new(shell);
    command.arg("-lic").arg(&script);
    let stdout = crate::updates::run_bounded_session(
        &mut command,
        timeout,
        "Checking whether Terminal can find phux timed out.",
        PROBE_STDOUT_CAP,
    )?;
    parse_probe(&stdout)
        .map_err(|()| anyhow::anyhow!("The login shell did not report a PATH probe"))
}

/// The profile and line for this process's login shell, using the installer's rules.
/// `from_shell` is the probe result. `None` means the probe failed, so
/// `ZDOTDIR`, `XDG_CONFIG_HOME`, and `PHUX_NO_MODIFY_PATH` fall back to this process.
pub(crate) fn profile_target(from_shell: Option<&LoginEnv>) -> Option<(PathBuf, String)> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    if !home.is_absolute() {
        return None;
    }
    let shell = std::env::var_os("SHELL").map_or_else(
        || "sh".to_owned(),
        |value| value.to_string_lossy().into_owned(),
    );
    let os = if cfg!(target_os = "macos") {
        "Darwin"
    } else {
        std::env::consts::OS
    };
    Some(profile_target_for(&home, &shell, from_shell, os))
}

pub(crate) fn profile_target_for(
    home: &Path,
    shell: &str,
    from_shell: Option<&LoginEnv>,
    os: &str,
) -> (PathBuf, String) {
    let (zdotdir, xdg) = match from_shell {
        Some(env) => (env.zdotdir.clone(), env.xdg_config.clone()),
        None => (
            std::env::var_os("ZDOTDIR").map(PathBuf::from),
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        ),
    };
    profile_for(shell, home, zdotdir.as_deref(), xdg.as_deref(), os)
}

/// Dismissal lives beside `updates.json` and is written only after the user closes the card.
pub(crate) fn is_dismissed() -> bool {
    dismissed_path().is_file()
}

pub(crate) fn dismiss() -> anyhow::Result<()> {
    let dir = crate::updates::config_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("Could not create {}", dir.display()))?;
    let path = dismissed_path();
    std::fs::write(&path, "1\n").with_context(|| format!("Could not save {}", path.display()))?;
    Ok(())
}

fn dismissed_path() -> PathBuf {
    crate::updates::config_dir().join("cli-path-dismissed")
}

/// `~/.zshrc` when `path` is under the process home.
pub(crate) fn home_display(path: &Path) -> String {
    let Ok(home) = std::env::var("HOME") else {
        return path.display().to_string();
    };
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicU64, Ordering};

    #[cfg(unix)]
    struct Fixture(PathBuf);
    #[cfg(unix)]
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "phux-shell-path-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn home(&self) -> PathBuf {
            let home = self.0.join("home");
            fs::create_dir_all(&home).unwrap();
            home
        }
        fn file(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }
        fn executable(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.file(name, contents);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
    }
    #[cfg(unix)]
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn profiles_follow_the_installer_for_each_shell() {
        let home = Path::new("/Users/ada");
        let (path, line) = profile_for("/bin/zsh", home, None, None, "Darwin");
        assert_eq!(path, home.join(".zshrc"));
        assert_eq!(line, "export PATH=/Users/ada/.local/bin:\"$PATH\"");

        let zdot = Path::new("/Users/ada/dot");
        let (path, line) = profile_for("zsh", home, Some(zdot), None, "Darwin");
        assert_eq!(path, zdot.join(".zshrc"));
        assert_eq!(line, "export PATH=/Users/ada/.local/bin:\"$PATH\"");

        let (path, _) = profile_for("/bin/bash", home, None, None, "Darwin");
        assert_eq!(path, home.join(".bash_profile"));
        let (path, _) = profile_for("/bin/bash", home, None, None, "Linux");
        assert_eq!(path, home.join(".bashrc"));

        let xdg = Path::new("/Users/ada/cfg");
        let (path, line) = profile_for("/usr/bin/fish", home, None, Some(xdg), "Darwin");
        assert_eq!(path, xdg.join("fish/conf.d/phux.fish"));
        assert_eq!(line, "fish_add_path -g /Users/ada/.local/bin");
        let (path, _) = profile_for("/opt/homebrew/bin/fish", home, None, None, "Linux");
        assert_eq!(path, home.join(".config/fish/conf.d/phux.fish"));

        let (path, line) = profile_for("/bin/sh", home, None, None, "Darwin");
        assert_eq!(path, home.join(".profile"));
        assert!(line.starts_with("export PATH="));
    }

    #[test]
    fn quoting_matches_the_installer_for_spaces_and_quotes() {
        assert_eq!(
            shell_quote("/Users/ada/.local/bin"),
            "/Users/ada/.local/bin"
        );
        let (_, line) = profile_for(
            "zsh",
            Path::new("/Users/ada/My Files"),
            None,
            None,
            "Darwin",
        );
        assert_eq!(
            line,
            "export PATH='/Users/ada/My Files/.local/bin':\"$PATH\""
        );
        let (_, line) = profile_for("bash", Path::new("/Users/o'brien"), None, None, "Linux");
        assert_eq!(
            line,
            "export PATH='/Users/o'\\''brien/.local/bin':\"$PATH\""
        );
        assert_eq!(shell_quote(""), "''");
    }

    #[cfg(unix)]
    #[test]
    fn appending_twice_yields_one_line_and_creates_parent_dirs_under_home() {
        let fixture = Fixture::new();
        let home = fixture.home();
        let (profile, line) =
            profile_for("fish", &home, None, Some(&home.join(".config")), "Linux");
        assert!(append_once_in(&home, &profile, INSTALLER_MARKER, &line, true).unwrap());
        assert!(!append_once_in(&home, &profile, INSTALLER_MARKER, &line, true).unwrap());
        let text = fs::read_to_string(&profile).unwrap();
        assert_eq!(text.lines().filter(|existing| *existing == line).count(), 1);
        assert_eq!(
            text.lines()
                .filter(|existing| *existing == INSTALLER_MARKER)
                .count(),
            1
        );
        assert!(text.starts_with('\n'));
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_installer_line_means_no_write() {
        let fixture = Fixture::new();
        let home = fixture.home();
        let (profile, line) = profile_for("zsh", &home, None, None, "Darwin");
        let original = format!("# already there\n{line}\n");
        fs::write(&profile, &original).unwrap();
        assert!(!append_once_in(&home, &profile, INSTALLER_MARKER, &line, true).unwrap());
        assert_eq!(fs::read_to_string(&profile).unwrap(), original);
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_profile_errors_without_a_partial_write() {
        let fixture = Fixture::new();
        let home = fixture.home();
        let profile = fixture.file("home/.zshrc", "keep\n");
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o444)).unwrap();
        let error = append_once_in(
            &home,
            &profile,
            INSTALLER_MARKER,
            "export PATH=/x:\"$PATH\"",
            true,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Could not update"), "{error}");
        assert_eq!(fs::read_to_string(&profile).unwrap(), "keep\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_outside_home_is_refused() {
        let fixture = Fixture::new();
        let home = fixture.home();
        let outside = fixture.file("outside/secret", "secret\n");
        let profile = home.join(".zshrc");
        std::os::unix::fs::symlink(&outside, &profile).unwrap();
        let error = append_once_in(
            &home,
            &profile,
            INSTALLER_MARKER,
            "export PATH=/x:\"$PATH\"",
            true,
        )
        .unwrap_err();
        assert!(error.to_string().contains("symlink outside"), "{error}");
        assert_eq!(fs::read_to_string(&outside).unwrap(), "secret\n");
    }

    #[cfg(unix)]
    #[test]
    fn an_ancestor_symlink_still_writes_inside_home() {
        let fixture = Fixture::new();
        let real = fixture.0.join("real");
        let home = real.join("home");
        fs::create_dir_all(&home).unwrap();
        let link = fixture.0.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let linked_home = link.join("home");
        let line = "export PATH=/x:\"$PATH\"";
        let profile = linked_home.join(".zshrc");
        append_once_in(&linked_home, &profile, INSTALLER_MARKER, line, true).unwrap();
        assert_eq!(
            fs::read_to_string(home.join(".zshrc"))
                .unwrap()
                .lines()
                .filter(|existing| *existing == line)
                .count(),
            1
        );
        let outside = link.join("not-home");
        let error =
            append_once_in(&linked_home, &outside, INSTALLER_MARKER, line, true).unwrap_err();
        assert!(error.to_string().contains("outside"), "{error}");
        assert!(!outside.exists());
        assert!(!real.join("not-home").exists());
    }

    #[cfg(unix)]
    #[test]
    fn path_changes_are_refused_when_modify_path_is_disabled() {
        let fixture = Fixture::new();
        let home = fixture.home();
        let profile = home.join(".profile");
        let error = append_once_in(
            &home,
            &profile,
            INSTALLER_MARKER,
            "export PATH=/x:\"$PATH\"",
            false,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "PHUX_NO_MODIFY_PATH is set");
        assert!(!profile.exists());
        assert!(!changes_allowed(Some(std::ffi::OsStr::new("1"))));
        assert!(changes_allowed(None));
        assert!(changes_allowed(Some(std::ffi::OsStr::new("0"))));
        assert!(changes_allowed(Some(std::ffi::OsStr::new(""))));
    }

    #[cfg(unix)]
    #[test]
    fn append_once_does_not_touch_a_profile_outside_the_process_home() {
        let fixture = Fixture::new();
        let profile = fixture.file("outside/.profile", "keep\n");
        let error =
            append_once(&profile, INSTALLER_MARKER, "export PATH=/x:\"$PATH\"").unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("outside") || message.contains("HOME"),
            "{message}"
        );
        assert_eq!(fs::read_to_string(&profile).unwrap(), "keep\n");
    }

    #[test]
    fn noisy_probe_output_parses_and_a_missing_sentinel_does_not() {
        let noisy = "welcome from rc\nrc noise PHUX_PATH_PROBE_BEGIN\nZDOTDIR=/Users/ada/.zsh\nXDG_CONFIG_HOME=\nPHUX_NO_MODIFY_PATH=1\n/Users/ada/.local/bin/phux\ndone PHUX_PATH_PROBE_END\n";
        let probe = parse_probe(noisy).unwrap();
        assert_eq!(
            probe.command.unwrap(),
            PathBuf::from("/Users/ada/.local/bin/phux")
        );
        assert_eq!(
            probe.env.zdotdir.as_deref(),
            Some(Path::new("/Users/ada/.zsh"))
        );
        assert_eq!(probe.env.xdg_config, None);
        assert_eq!(
            probe.env.modify_path.as_deref(),
            Some(std::ffi::OsStr::new("1"))
        );
        let (profile, _) =
            profile_target_for(Path::new("/Users/ada"), "zsh", Some(&probe.env), "Darwin");
        assert_eq!(profile, PathBuf::from("/Users/ada/.zsh/.zshrc"));
        assert!(!changes_allowed(probe.env.modify_path.as_deref()));
        let empty = "noise PHUX_PATH_PROBE_BEGIN\n\ndone PHUX_PATH_PROBE_END\nmore\n";
        assert_eq!(parse_probe(empty).unwrap().command, None);
        assert!(parse_probe("PHUX_PATH_PROBE_BEGIN\n/bin/phux\n").is_err());
        assert!(parse_probe("no sentinels\n").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resolves_to_compares_canonical_paths() {
        let fixture = Fixture::new();
        let cli = fixture.executable("bin/phux", "#!/bin/sh\n");
        let link = fixture.0.join("link");
        std::os::unix::fs::symlink(&cli, &link).unwrap();
        assert!(resolves_to(&link, &cli));
        assert!(!resolves_to(Path::new("/missing/phux"), &cli));
    }

    #[cfg(unix)]
    #[test]
    fn a_timeout_means_not_on_path() {
        let fixture = Fixture::new();
        let shell = fixture.executable("sleep-shell", "#!/bin/sh\nsleep 30\n");
        let cli = fixture.executable("bin/phux", "#!/bin/sh\n");
        let started = std::time::Instant::now();
        let result = probe_login_shell(&shell, Duration::from_millis(400));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "probe waited for sleep"
        );
        assert!(result.is_err());
        assert_eq!(
            classify(result.map(|probe| probe.command), &cli),
            PathStatus::Unverified
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_stub_shell_probe_reads_command_v_through_noise() {
        let fixture = Fixture::new();
        let cli = fixture.executable("bin/phux", "#!/bin/sh\n");
        let bin = cli.parent().unwrap().display().to_string();
        let shell = fixture.executable(
            "stub-shell",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' 'welcome from rc'\nPATH={bin}:/usr/bin:/bin\nexport PATH\nif [ \"$1\" = -lic ]; then /bin/sh -c \"$2\"; fi\nprintf '%s\\n' 'done from rc'\n"
            ),
        );
        let found = probe_login_shell(&shell, Duration::from_secs(3))
            .unwrap()
            .command
            .expect("command -v printed a path");
        assert!(resolves_to(&found, &cli), "{found:?}");
        assert_eq!(classify(Ok(Some(found)), &cli), PathStatus::OnPath);
        let missing = fixture.executable(
            "missing-shell",
            "#!/bin/sh\nprintf '%s\\n' 'welcome from rc'\nPATH=/usr/bin:/bin\nexport PATH\nif [ \"$1\" = -lic ]; then /bin/sh -c \"$2\"; fi\n",
        );
        let result = probe_login_shell(&missing, Duration::from_secs(3)).unwrap();
        assert_eq!(result.command, None);
        assert_eq!(classify(Ok(result.command), &cli), PathStatus::Missing);
    }

    #[cfg(unix)]
    #[test]
    fn probe_reads_the_login_shell_environment_not_the_app() {
        let fixture = Fixture::new();
        let shell = fixture.executable(
            "env-shell",
            "#!/bin/sh\nexport ZDOTDIR=/from/the/shell\nexport XDG_CONFIG_HOME=/from/the/xdg\nexport PHUX_NO_MODIFY_PATH=1\nif [ \"$1\" = -lic ]; then /bin/sh -c \"$2\"; fi\n",
        );
        let probe = probe_login_shell(&shell, Duration::from_secs(3)).unwrap();
        assert_eq!(
            probe.env.zdotdir.as_deref(),
            Some(Path::new("/from/the/shell"))
        );
        assert_eq!(
            probe.env.xdg_config.as_deref(),
            Some(Path::new("/from/the/xdg"))
        );
        assert!(!changes_allowed(probe.env.modify_path.as_deref()));
        let (profile, line) =
            profile_target_for(Path::new("/Users/ada"), "zsh", Some(&probe.env), "Darwin");
        assert_eq!(profile, PathBuf::from("/from/the/shell/.zshrc"));
        assert!(line.contains("/Users/ada/.local/bin"));
        let (fish, _) =
            profile_target_for(Path::new("/Users/ada"), "fish", Some(&probe.env), "Darwin");
        assert_eq!(fish, PathBuf::from("/from/the/xdg/fish/conf.d/phux.fish"));
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_profile_is_not_replaced() {
        let fixture = Fixture::new();
        let home = fixture.home();
        let missing = home.join("missing-target");
        let profile = home.join(".zshrc");
        std::os::unix::fs::symlink(&missing, &profile).unwrap();
        let error = append_once_in(
            &home,
            &profile,
            INSTALLER_MARKER,
            "export PATH=/x:\"$PATH\"",
            true,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("symlink") || error.to_string().contains("resolve"),
            "{error}"
        );
        assert!(profile.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(!missing.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_timeout_kills_the_probe_process_group() {
        let fixture = Fixture::new();
        let marker = fixture.0.join("still-alive");
        let shell = fixture.executable(
            "group-shell",
            &format!(
                "#!/bin/sh\n(sleep 1; echo live > '{}') &\nsleep 30\n",
                marker.display()
            ),
        );
        let started = std::time::Instant::now();
        let result = probe_login_shell(&shell, Duration::from_millis(400));
        let message = result.unwrap_err().to_string();
        assert!(message.contains("timed out"), "{message}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "probe waited for the grandchild"
        );
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!marker.exists(), "timeout left a live probe descendant");
    }

    #[cfg(unix)]
    #[test]
    fn probe_stdout_is_capped_and_trailing_noise_still_parses() {
        let fixture = Fixture::new();
        let leading = fixture.executable(
            "leading-noise",
            "#!/bin/sh\nyes A | head -c 81920\nprintf '\\n'\nif [ \"$1\" = -lic ]; then /bin/sh -c \"$2\"; fi\n",
        );
        let hidden = probe_login_shell(&leading, Duration::from_secs(3)).unwrap_err();
        assert!(hidden.to_string().contains("did not report"), "{hidden}");
        let trailing = fixture.executable(
            "trailing-noise",
            "#!/bin/sh\nif [ \"$1\" = -lic ]; then /bin/sh -c \"$2\"; fi\nyes A | head -c 81920\n",
        );
        let probe = probe_login_shell(&trailing, Duration::from_secs(3)).unwrap();
        assert_eq!(probe.command, None);
    }
}
