//! Fetch a classified plugin source into a staging directory and move the
//! plugin root into its managed location.
//!
//! Git clones are shallow snapshots: `.git` is removed before the plugin
//! root is promoted, so the managed tree matches a directory install
//! (ADR-0041). A subdirectory is walked component by component and refused
//! when any component is a symlink, so the plugin root stays inside the clone.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::super::registry::temp_nonce;
use super::git;
use super::source::InstallSource;

/// Fetch `source` into the (not yet existing) `staging` directory.
/// Returns the resolved commit hash for git sources.
pub(super) fn fetch_into_staging(
    source: &InstallSource,
    staging: &Path,
) -> Result<Option<String>, String> {
    match source {
        InstallSource::Dir(src) => {
            copy_tree(src, staging)?;
            Ok(None)
        }
        InstallSource::Tarball(file) => {
            std::fs::create_dir_all(staging)
                .map_err(|err| format!("could not create {}: {err}", staging.display()))?;
            let mut cmd = Command::new("tar");
            cmd.arg("-xf").arg(file).arg("-C").arg(staging);
            run_tool(cmd, "tar extract")?;
            Ok(None)
        }
        InstallSource::Git { url, branch, .. } => {
            // `.git` stays until [`seal_git_checkout`] has checked history.
            let rev = git::clone_at(url, branch.as_deref(), staging)?;
            Ok(Some(rev))
        }
    }
}

/// The plugin root inside a fetched tree: `subdir` when the source names
/// one, otherwise the staging directory or its single top-level directory.
pub(super) fn locate_fetched_root(staging: &Path, subdir: Option<&str>) -> Result<PathBuf, String> {
    subdir.map_or_else(
        || locate_plugin_root(staging),
        |subdir| subdir_root(staging, subdir),
    )
}

/// How an update treats the commit already recorded in the lockfile.
pub(super) struct PinCheck<'a> {
    /// Resolved commit from the last install or update.
    pub(super) previous: &'a str,
    /// Branch or tag name, when the install was not pinned to a commit.
    pub(super) branch: Option<&'a str>,
    /// The install was requested by commit id (`--rev` is hexadecimal).
    pub(super) pinned_sha: bool,
    /// `--latest`: accept a rewritten history or move off a pinned commit.
    pub(super) latest: bool,
}

/// Refuse symlinks in the plugin root, enforce the recorded commit, then
/// drop `.git` so the promoted tree is a snapshot.
pub(super) fn seal_git_checkout(
    staging: &Path,
    plugin_root: &Path,
    new_rev: &str,
    pin: Option<PinCheck<'_>>,
) -> Result<(), String> {
    refuse_symlinks(plugin_root)?;
    if let Some(pin) = pin {
        enforce_pin(staging, new_rev, &pin)?;
    }
    let git_dir = staging.join(".git");
    if git_dir.exists() {
        std::fs::remove_dir_all(&git_dir)
            .map_err(|err| format!("could not remove {}: {err}", git_dir.display()))?;
    }
    Ok(())
}

fn enforce_pin(repo: &Path, new_rev: &str, pin: &PinCheck<'_>) -> Result<(), String> {
    if pin.previous.eq_ignore_ascii_case(new_rev) {
        return Ok(());
    }
    if pin.pinned_sha && !pin.latest {
        return Err(format!(
            "pinned commit {} would move to {new_rev}; pass --latest to move off that commit",
            pin.previous
        ));
    }
    if pin.latest {
        return Ok(());
    }
    let ancestry = git::is_descendant(repo, pin.previous, new_rev, pin.branch)?;
    if ancestry.descendant {
        return Ok(());
    }
    let mut message = format!(
        "commit {new_rev} is not a descendant of recorded commit {}. \
         The check looked {} commits deep and the history may be deeper; \
         pass --latest to accept this update",
        pin.previous,
        git::ANCESTRY_DEPTH,
    );
    if let Some(err) = ancestry.fetch_error {
        message.push_str(". ");
        message.push_str(&err);
    }
    Err(message)
}

/// Walk `root`, skipping `.git`, and refuse every symlink. The same rule as
/// a local directory install.
pub(super) fn refuse_symlinks(root: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(root)
        .map_err(|err| format!("could not read {}: {err}", root.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("could not read {}: {err}", root.display()))?;
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| format!("could not inspect {}: {err}", path.display()))?;
        reject_symlink(&path, file_type)?;
        if file_type.is_dir() {
            refuse_symlinks(&path)?;
        }
    }
    Ok(())
}

fn reject_symlink(path: &Path, file_type: std::fs::FileType) -> Result<(), String> {
    if file_type.is_symlink() {
        Err(format!(
            "{} is a symlink; plugin installs must be self-contained",
            path.display()
        ))
    } else {
        Ok(())
    }
}

/// Move the fetched plugin root into its final managed directory
/// (fresh install: the destination must not exist).
pub(super) fn promote(staging: &Path, plugin_root: &Path, final_dir: &Path) -> Result<(), String> {
    std::fs::rename(plugin_root, final_dir).map_err(|err| {
        format!(
            "could not move {} to {}: {err}",
            plugin_root.display(),
            final_dir.display()
        )
    })?;
    if plugin_root != staging {
        std::fs::remove_dir_all(staging)
            .map_err(|err| format!("could not clean up {}: {err}", staging.display()))?;
    }
    Ok(())
}

/// Replace an existing install with the freshly fetched tree, keeping a
/// backup alongside so a failed swap restores the previous version.
pub(super) fn swap_install_dir(
    plugins_dir: &Path,
    staging: &Path,
    plugin_root: &Path,
    final_dir: &Path,
) -> Result<(), String> {
    let backup = plugins_dir.join(format!(".backup-{}-{}", std::process::id(), temp_nonce()));
    let had_previous = final_dir.exists();
    if had_previous {
        std::fs::rename(final_dir, &backup).map_err(|err| {
            format!(
                "could not set aside {}: {err} (previous install left untouched)",
                final_dir.display()
            )
        })?;
    }
    if let Err(err) = std::fs::rename(plugin_root, final_dir) {
        let restore = if had_previous {
            match std::fs::rename(&backup, final_dir) {
                Ok(()) => " (previous install restored)".to_owned(),
                Err(restore_err) => {
                    format!(
                        " (previous install stranded at {}: {restore_err})",
                        backup.display()
                    )
                }
            }
        } else {
            String::new()
        };
        return Err(format!(
            "could not move {} to {}: {err}{restore}",
            plugin_root.display(),
            final_dir.display()
        ));
    }
    if had_previous {
        let _ = std::fs::remove_dir_all(&backup);
    }
    if plugin_root != staging {
        let _ = std::fs::remove_dir_all(staging);
    }
    Ok(())
}

/// Staging directory that removes itself on drop unless disarmed, so an
/// aborted install/update never strands a half-fetched tree.
pub(super) struct StagingGuard {
    path: PathBuf,
    armed: bool,
}

impl StagingGuard {
    pub(super) fn claim(plugins_dir: &Path) -> Self {
        let path = plugins_dir.join(format!(".staging-{}-{}", std::process::id(), temp_nonce()));
        Self { path, armed: true }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) const fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Run one child process to completion, failing loudly with its stderr.
fn run_tool(mut cmd: Command, what: &str) -> Result<String, String> {
    let output = cmd
        .output()
        .map_err(|err| format!("could not spawn {what}: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{what} failed ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Copy a plugin source tree, skipping `.git` and refusing symlinks (a
/// managed install must be a self-contained snapshot).
fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst)
        .map_err(|err| format!("could not create {}: {err}", dst.display()))?;
    let entries =
        std::fs::read_dir(src).map_err(|err| format!("could not read {}: {err}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("could not read {}: {err}", src.display()))?;
        if entry.file_name() == ".git" {
            continue;
        }
        let from = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|err| format!("could not inspect {}: {err}", from.display()))?;
        reject_symlink(&from, file_type)?;
        let to = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .map_err(|err| format!("could not copy {}: {err}", from.display()))?;
        }
    }
    Ok(())
}

/// Find the fetched package's plugin root: the staging dir itself, or a
/// single top-level directory (the usual tarball / repo layout).
fn locate_plugin_root(staging: &Path) -> Result<PathBuf, String> {
    if staging.join("phux-plugin.toml").is_file() {
        return Ok(staging.to_path_buf());
    }
    let entries = std::fs::read_dir(staging)
        .map_err(|err| format!("could not read {}: {err}", staging.display()))?;
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| format!("could not read {}: {err}", staging.display()))?;
        if entry.path().is_dir() {
            dirs.push(entry.path());
        }
    }
    if let [only] = dirs.as_slice()
        && only.join("phux-plugin.toml").is_file()
    {
        return Ok(only.clone());
    }
    Err("fetched package does not contain a phux-plugin.toml at its root".to_owned())
}

/// `staging/subdir`, refusing `.`, `..`, and any symlink component.
fn subdir_root(staging: &Path, subdir: &str) -> Result<PathBuf, String> {
    let mut root = staging.to_path_buf();
    for segment in subdir.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(format!("subdirectory {subdir:?} is not inside the clone"));
        }
        root.push(segment);
        let meta = std::fs::symlink_metadata(&root).map_err(|err| {
            format!("subdirectory {subdir} of the clone is missing ({segment}): {err}")
        })?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "subdirectory {subdir} of the clone contains a symlink ({segment}); \
                 plugin installs must be self-contained"
            ));
        }
        if !meta.is_dir() {
            return Err(format!(
                "subdirectory {subdir} of the clone is not a directory"
            ));
        }
    }
    if root.join("phux-plugin.toml").is_file() {
        Ok(root)
    } else {
        Err(format!(
            "subdirectory {subdir} of the clone has no phux-plugin.toml"
        ))
    }
}
