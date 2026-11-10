//! The `starter` and `crew` distros, compiled into the binary.
//!
//! An installed release has no source checkout, so `phux config distro use
//! crew` would find nothing. When no search directory has `crew`, phux writes
//! these files under the data dir (`<data>/phux/distros/…`, plus the plugins
//! `starter` names at `../../plugins/…`), which is the second place distro
//! names are looked up. A copy phux wrote carries a marker and is rewritten
//! when the binary's version changes; a directory without the marker is the
//! user's and is never touched.

use std::path::{Path, PathBuf};

use phux_config::distro::resolve_distro_in;

/// Marks a directory phux wrote; its contents name the writing version.
const MARKER: &str = ".phux-bundled";

macro_rules! bundle {
    ($($path:literal),+ $(,)?) => {
        &[$(($path, include_str!(concat!("../../../../../", $path)))),+]
    };
}

/// Repo-relative path and contents of every bundled file.
const FILES: &[(&str, &str)] = bundle![
    "distros/crew/README.md",
    "distros/crew/crew.py",
    "distros/crew/crew.toml",
    "distros/crew/integrations/crew.toml",
    "distros/crew/lead.md",
    "distros/crew/phux-plugin.toml",
    "distros/crew/skills/crew-pulse/SKILL.md",
    "distros/crew/skills/crew-scout/SKILL.md",
    "distros/crew/skills/crew-ship/SKILL.md",
    "distros/starter/README.md",
    "distros/starter/starter.toml",
    "plugins/agent-tools/README.md",
    "plugins/agent-tools/config/phux/config.toml",
    "plugins/agent-tools/integrations/amp.toml",
    "plugins/agent-tools/integrations/claude-code.toml",
    "plugins/agent-tools/integrations/codex.toml",
    "plugins/agent-tools/integrations/cursor-agent.toml",
    "plugins/agent-tools/integrations/gemini-cli.toml",
    "plugins/agent-tools/integrations/generic-shell-agent.toml",
    "plugins/agent-tools/integrations/goose.toml",
    "plugins/agent-tools/integrations/grok.toml",
    "plugins/agent-tools/integrations/omp.toml",
    "plugins/agent-tools/integrations/opencode.toml",
    "plugins/agent-tools/integrations/pi.toml",
    "plugins/agent-tools/phux-plugin.toml",
    "plugins/agent-tools/scripts/bench-common.sh",
    "plugins/agent-tools/scripts/detect-agents.sh",
    "plugins/agent-tools/scripts/drive-bench.sh",
    "plugins/agent-tools/scripts/inspect.sh",
    "plugins/agent-tools/scripts/integration-common.sh",
    "plugins/agent-tools/scripts/launch-bench.sh",
    "plugins/agent-tools/scripts/link-integration.sh",
    "plugins/agent-tools/scripts/list-bench.sh",
    "plugins/agent-tools/scripts/list-integrations.sh",
    "plugins/agent-tools/scripts/phux-agent-wrap.sh",
    "plugins/agent-tools/scripts/phux-grok-wrap.sh",
    "plugins/agent-tools/scripts/smoke-agent-wrap.sh",
    "plugins/agent-tools/scripts/smoke-integrations.sh",
    "plugins/agent-tools/scripts/status-integrations.sh",
    "plugins/agent-tools/scripts/unlink-integration.sh",
    "plugins/agent-tools/scripts/validate-integrations.sh",
    "plugins/continuum/README.md",
    "plugins/continuum/config/phux/config.toml",
    "plugins/continuum/phux-plugin.toml",
    "plugins/continuum/scripts/autosave.sh",
    "plugins/continuum/scripts/restore-latest.sh",
    "plugins/usage/README.md",
    "plugins/usage/phux-plugin.toml",
    "plugins/usage/scripts/publish.sh",
    "plugins/usage/scripts/show.sh",
];

/// `<data>/phux`, the root the bundle is written under (its `distros/` is a
/// distro search directory).
fn data_root() -> Option<PathBuf> {
    phux_config::theme::themes_dir().and_then(|themes| themes.parent().map(Path::to_path_buf))
}

/// Write the bundle when no search directory provides `crew`, or refresh a
/// copy an older phux wrote. A no-op on a dev build, whose checkout has both.
pub(super) fn ensure(dirs: &[PathBuf]) -> Result<(), String> {
    let Some(root) = data_root() else {
        return Ok(());
    };
    ensure_in(&root, dirs, env!("PHUX_VERSION_LABEL"))
}

fn ensure_in(root: &Path, dirs: &[PathBuf], version: &str) -> Result<(), String> {
    let ours = root.join("distros/crew").join(MARKER).is_file();
    if !ours && resolve_distro_in("crew", dirs).is_ok() {
        return Ok(());
    }
    let stamp = format!(
        "Written by phux {version}, which rewrites this directory when its version changes.\n\
         Delete this file to keep your own edits.\n"
    );
    let mut units: Vec<&str> = FILES.iter().map(|(path, _)| unit(path)).collect();
    units.dedup();
    for unit in units {
        let dir = root.join(unit);
        let marker = dir.join(MARKER);
        match std::fs::read_to_string(&marker) {
            Ok(old) if old == stamp => continue,
            Ok(_) => remove_dir(&dir)?,
            Err(_) if dir.exists() => continue, // the user's own copy
            Err(_) => {}
        }
        for (path, text) in FILES.iter().filter(|(path, _)| self::unit(path) == unit) {
            write(&root.join(path), text)?;
        }
        write(&marker, &stamp)?;
    }
    Ok(())
}

/// `distros/crew/lead.md` -> `distros/crew`.
fn unit(path: &str) -> &str {
    let end = path
        .match_indices('/')
        .nth(1)
        .map_or(path.len(), |(at, _)| at);
    &path[..end]
}

fn remove_dir(dir: &Path) -> Result<(), String> {
    std::fs::remove_dir_all(dir)
        .map_err(|err| format!("could not refresh {}: {err}", dir.display()))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    let fail = |err: std::io::Error| format!("could not write {}: {err}", path.display());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(fail)?;
    }
    std::fs::write(path, text).map_err(fail)?;
    #[cfg(unix)]
    if path
        .extension()
        .is_some_and(|ext| ext == "sh" || ext == "py")
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(fail)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FILES, MARKER, ensure_in, unit};
    use std::path::{Path, PathBuf};

    /// The checkout, read at run time (`cargo test` and nextest export it).
    fn repo() -> PathBuf {
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..")
    }

    /// Every shipped file of each bundled directory is embedded (tests
    /// excepted), so a new file cannot be left out of releases.
    #[test]
    fn bundle_lists_every_file() {
        fn walk(repo: &Path, dir: &Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if name.starts_with('.') || name == "__pycache__" || name.starts_with("test_") {
                    continue;
                }
                if path.is_dir() {
                    walk(repo, &path, out);
                } else {
                    let rel = path.strip_prefix(repo).unwrap();
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        let mut units: Vec<&str> = FILES.iter().map(|(path, _)| unit(path)).collect();
        units.dedup();
        let mut on_disk = Vec::new();
        for unit in units {
            walk(&repo(), &repo().join(unit), &mut on_disk);
        }
        on_disk.sort();
        let mut embedded: Vec<String> = FILES.iter().map(|(path, _)| (*path).to_owned()).collect();
        embedded.sort();
        assert_eq!(embedded, on_disk);
    }

    #[test]
    fn writes_when_missing_refreshes_its_own_and_keeps_the_users() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("phux");
        let dirs = vec![root.join("distros")];

        ensure_in(&root, &dirs, "1.0.0").unwrap();
        let lead = root.join("distros/crew/lead.md");
        assert!(lead.is_file());
        assert!(root.join("plugins/agent-tools/phux-plugin.toml").is_file());
        // starter's `../../plugins/...` manifests resolve from the written layout.
        assert!(
            root.join("distros/starter/../../plugins/continuum/phux-plugin.toml")
                .is_file()
        );
        assert!(
            phux_config::distro::resolve_distro_in("crew", &dirs).is_ok(),
            "crew resolves by name from the data dir"
        );

        // A new version rewrites its own copy and drops files it no longer ships.
        let stale = root.join("distros/crew/gone.md");
        std::fs::write(&stale, "old").unwrap();
        ensure_in(&root, &dirs, "1.0.1").unwrap();
        assert!(!stale.exists());
        assert!(
            std::fs::read_to_string(root.join("distros/crew").join(MARKER))
                .unwrap()
                .contains("1.0.1")
        );

        // Removing the marker hands the directory to the user.
        std::fs::remove_file(root.join("distros/crew").join(MARKER)).unwrap();
        std::fs::write(&lead, "mine").unwrap();
        ensure_in(&root, &dirs, "1.0.2").unwrap();
        assert_eq!(std::fs::read_to_string(&lead).unwrap(), "mine");
    }

    #[test]
    fn a_checkout_crew_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("phux");
        let dirs = vec![root.join("distros"), repo().join("distros")];
        ensure_in(&root, &dirs, "1.0.0").unwrap();
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn scripts_are_executable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("phux");
        ensure_in(&root, &[], "1.0.0").unwrap();
        let mode = std::fs::metadata(root.join("plugins/agent-tools/scripts/phux-agent-wrap.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }
}
