//! `phux plugin install` / `phux plugin update` (phux-r82.8).
//!
//! Installs are source-agnostic fetches into one managed directory:
//! a git URL (cloned with the system `git`), a local plugin directory
//! (copied), or a local tarball (extracted with the system `tar`) lands
//! under the phux data dir (`$XDG_DATA_HOME/phux/plugins`, else
//! `~/.local/share/phux/plugins`). The fetched manifest's `[[build]]`
//! steps for this platform then run as child processes with a bounded
//! timeout and captured output (the phux-plugin runtime contract), the
//! manifest is validated — including the `min_phux_version` gate — and
//! the package is linked into `config.toml` through the same registry
//! path as `phux plugin link`. Provenance (ref, branch, resolved commit)
//! is recorded in the managed dir's `plugins.lock` so `phux plugin
//! update` can re-fetch without re-asking the user.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_config::plugin::{self, PluginManifest, PluginPlatform};
use phux_plugin::{CommandSpec, PluginActionOutcome, run_command_spec};

use super::lock::{
    PluginLockEntry, PluginSourceKind, lockfile_path, read_lockfile, upsert_entry, write_lockfile,
};
use super::registry::temp_nonce;
use super::{fail, json::print_json, upsert_config_entry};

/// Per-build-step wall-clock bound. A plugin build that cannot finish in
/// five minutes should ship prebuilt artifacts instead of hanging the CLI.
const BUILD_TIMEOUT: Duration = Duration::from_secs(300);

#[cfg(target_os = "linux")]
const CURRENT_PLATFORM: Option<PluginPlatform> = Some(PluginPlatform::Linux);
#[cfg(target_os = "macos")]
const CURRENT_PLATFORM: Option<PluginPlatform> = Some(PluginPlatform::Macos);
#[cfg(target_os = "windows")]
const CURRENT_PLATFORM: Option<PluginPlatform> = Some(PluginPlatform::Windows);
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const CURRENT_PLATFORM: Option<PluginPlatform> = None;

/// `phux plugin install REF [--rev REV] [--disabled] [--json]`.
pub(super) fn run_install(
    reference: &str,
    rev: Option<&str>,
    disabled: bool,
    json: bool,
) -> ExitCode {
    let runtime = match crate::commands::cli_runtime() {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    match install(&runtime, reference, rev, !disabled, json) {
        Ok(code) => code,
        Err(err) => fail(&err),
    }
}

/// `phux plugin update [NAME] [--json]`.
pub(super) fn run_update(name: Option<&str>, json: bool) -> ExitCode {
    let runtime = match crate::commands::cli_runtime() {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    match update(&runtime, name, json) {
        Ok(code) => code,
        Err(err) => fail(&err),
    }
}

/// Where a `phux plugin install` refers its package from.
#[derive(Debug, Clone)]
enum InstallSource {
    /// Clone `url` with the system `git`, optionally at a branch or tag.
    Git { url: String, branch: Option<String> },
    /// Copy a local plugin directory.
    Dir(PathBuf),
    /// Extract a local tarball with the system `tar`.
    Tarball(PathBuf),
}

impl InstallSource {
    const fn kind(&self) -> PluginSourceKind {
        match self {
            Self::Git { .. } => PluginSourceKind::Git,
            Self::Dir(_) => PluginSourceKind::Dir,
            Self::Tarball(_) => PluginSourceKind::Tarball,
        }
    }

    fn source_ref(&self) -> String {
        match self {
            Self::Git { url, .. } => url.clone(),
            Self::Dir(path) | Self::Tarball(path) => path.display().to_string(),
        }
    }

    fn branch(&self) -> Option<String> {
        match self {
            Self::Git { branch, .. } => branch.clone(),
            Self::Dir(_) | Self::Tarball(_) => None,
        }
    }
}

fn install(
    runtime: &tokio::runtime::Runtime,
    reference: &str,
    rev: Option<&str>,
    enabled: bool,
    json: bool,
) -> Result<ExitCode, String> {
    let source = classify_source(reference, rev)?;
    let plugins_dir = plugins_data_dir()?;
    std::fs::create_dir_all(&plugins_dir)
        .map_err(|err| format!("could not create {}: {err}", plugins_dir.display()))?;

    let mut staging = StagingGuard::claim(&plugins_dir);
    let resolved_rev = fetch_into_staging(&source, staging.path())?;
    let plugin_root = locate_plugin_root(staging.path())?;
    let manifest = load_fetched_manifest(&plugin_root, reference)?;
    run_build_steps(runtime, &manifest)?;

    let final_dir = plugins_dir.join(install_dir_name(&manifest.id));
    if final_dir.exists() {
        return Err(format!(
            "plugin {} is already installed at {}; run `phux plugin update {}` to refresh it",
            manifest.id,
            final_dir.display(),
            manifest.id,
        ));
    }
    promote(staging.path(), &plugin_root, &final_dir)?;
    staging.disarm();

    // Reload from the managed location so the linked manifest path (and
    // the plugin root actions execute from) is the installed copy.
    let manifest = load_fetched_manifest(&final_dir, reference)?;

    let mut lockfile = read_lockfile(&plugins_dir)?;
    upsert_entry(
        &mut lockfile,
        PluginLockEntry {
            id: manifest.id.clone(),
            source: source.kind(),
            source_ref: source.source_ref(),
            branch: source.branch(),
            rev: resolved_rev.clone(),
        },
    );
    write_lockfile(&plugins_dir, &lockfile)?;

    let entry = upsert_config_entry(manifest, enabled)?;
    if json {
        let doc = serde_json::json!({
            "schema_version": 1,
            "installed": {
                "id": entry.manifest.id,
                "version": entry.manifest.version,
                "dir": final_dir,
                "source": source.kind().to_string(),
                "ref": source.source_ref(),
                "branch": source.branch(),
                "rev": resolved_rev,
                "enabled": entry.enabled,
            },
        });
        return Ok(print_json(&doc));
    }
    let state = if entry.enabled { "enabled" } else { "disabled" };
    let rev_note = resolved_rev
        .as_deref()
        .map_or_else(String::new, |rev| format!(" at {rev}"));
    outln!(
        "installed {} {}{rev_note} -> {} ({state})",
        entry.manifest.id,
        entry.manifest.version,
        final_dir.display(),
    );
    Ok(ExitCode::SUCCESS)
}

/// One `phux plugin update` result line.
struct UpdateReport {
    id: String,
    version: String,
    rev: Option<String>,
}

fn update(
    runtime: &tokio::runtime::Runtime,
    name: Option<&str>,
    json: bool,
) -> Result<ExitCode, String> {
    let plugins_dir = plugins_data_dir()?;
    let mut lockfile = read_lockfile(&plugins_dir)?;
    let targets: Vec<usize> = match name {
        Some(name) => {
            let index = lockfile
                .plugins
                .iter()
                .position(|entry| entry.id == name)
                .ok_or_else(|| {
                    format!(
                        "plugin {name:?} is not recorded in {}",
                        lockfile_path(&plugins_dir).display()
                    )
                })?;
            vec![index]
        }
        None => (0..lockfile.plugins.len()).collect(),
    };

    let mut reports = Vec::with_capacity(targets.len());
    for index in targets {
        let entry = lockfile.plugins[index].clone();
        let report = update_one(runtime, &plugins_dir, &entry)?;
        lockfile.plugins[index].rev.clone_from(&report.rev);
        write_lockfile(&plugins_dir, &lockfile)?;
        reports.push(report);
    }

    if json {
        let updated: Vec<_> = reports
            .iter()
            .map(|report| {
                serde_json::json!({
                    "id": report.id,
                    "version": report.version,
                    "rev": report.rev,
                })
            })
            .collect();
        let doc = serde_json::json!({
            "schema_version": 1,
            "updated": updated,
        });
        return Ok(print_json(&doc));
    }
    if reports.is_empty() {
        outln!("no managed plugins installed");
        return Ok(ExitCode::SUCCESS);
    }
    for report in reports {
        let rev_note = report
            .rev
            .as_deref()
            .map_or_else(String::new, |rev| format!(" at {rev}"));
        outln!("updated {} {}{rev_note}", report.id, report.version);
    }
    Ok(ExitCode::SUCCESS)
}

fn update_one(
    runtime: &tokio::runtime::Runtime,
    plugins_dir: &Path,
    entry: &PluginLockEntry,
) -> Result<UpdateReport, String> {
    let source = source_from_lock(entry)?;
    let mut staging = StagingGuard::claim(plugins_dir);
    let resolved_rev = fetch_into_staging(&source, staging.path())?;
    let plugin_root = locate_plugin_root(staging.path())?;
    let manifest = load_fetched_manifest(&plugin_root, &entry.source_ref)?;
    if manifest.id != entry.id {
        return Err(format!(
            "source {} now provides plugin id {:?} (expected {:?}); refusing to update",
            entry.source_ref, manifest.id, entry.id,
        ));
    }
    run_build_steps(runtime, &manifest)?;

    let final_dir = plugins_dir.join(install_dir_name(&entry.id));
    swap_install_dir(plugins_dir, staging.path(), &plugin_root, &final_dir)?;
    staging.disarm();
    Ok(UpdateReport {
        id: entry.id.clone(),
        version: manifest.version,
        rev: resolved_rev,
    })
}

/// The managed plugins directory under the phux data dir:
/// `$XDG_DATA_HOME/phux/plugins`, else `~/.local/share/phux/plugins`.
fn plugins_data_dir() -> Result<PathBuf, String> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("phux").join("plugins"));
    }
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| {
            "cannot resolve the phux data dir: neither XDG_DATA_HOME nor HOME is set".to_owned()
        })?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("phux")
        .join("plugins"))
}

fn classify_source(reference: &str, rev: Option<&str>) -> Result<InstallSource, String> {
    let path = Path::new(reference);
    if path.is_dir() {
        reject_rev_for_local(rev)?;
        return Ok(InstallSource::Dir(canonicalize(path)?));
    }
    if path.is_file() {
        reject_rev_for_local(rev)?;
        if is_tarball_name(reference) {
            return Ok(InstallSource::Tarball(canonicalize(path)?));
        }
        return Err(format!(
            "{reference} is a file but not a recognized tarball (.tar, .tar.gz, .tgz)"
        ));
    }
    if reference.contains("://") || reference.starts_with("git@") {
        return Ok(InstallSource::Git {
            url: reference.to_owned(),
            branch: rev.map(str::to_owned),
        });
    }
    Err(format!(
        "{reference:?} is neither an existing local path nor a git URL"
    ))
}

fn reject_rev_for_local(rev: Option<&str>) -> Result<(), String> {
    if rev.is_some() {
        return Err("--rev only applies to git sources".to_owned());
    }
    Ok(())
}

fn source_from_lock(entry: &PluginLockEntry) -> Result<InstallSource, String> {
    match entry.source {
        PluginSourceKind::Git => Ok(InstallSource::Git {
            url: entry.source_ref.clone(),
            branch: entry.branch.clone(),
        }),
        PluginSourceKind::Dir => {
            let path = PathBuf::from(&entry.source_ref);
            if path.is_dir() {
                Ok(InstallSource::Dir(path))
            } else {
                Err(format!(
                    "recorded source directory {} for plugin {} no longer exists",
                    path.display(),
                    entry.id,
                ))
            }
        }
        PluginSourceKind::Tarball => {
            let path = PathBuf::from(&entry.source_ref);
            if path.is_file() {
                Ok(InstallSource::Tarball(path))
            } else {
                Err(format!(
                    "recorded source tarball {} for plugin {} no longer exists",
                    path.display(),
                    entry.id,
                ))
            }
        }
    }
}

fn is_tarball_name(name: &str) -> bool {
    // Deliberately exact-case: install refs are user-typed paths, and
    // the managed contract documents the lowercase extensions.
    #[allow(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "documented lowercase tarball extensions"
    )]
    {
        name.ends_with(".tar") || name.ends_with(".tar.gz") || name.ends_with(".tgz")
    }
}

fn canonicalize(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|err| format!("could not resolve {}: {err}", path.display()))
}

/// Fetch `source` into the (not yet existing) `staging` directory.
/// Returns the resolved commit hash for git sources.
fn fetch_into_staging(source: &InstallSource, staging: &Path) -> Result<Option<String>, String> {
    match source {
        InstallSource::Dir(src) => {
            copy_tree(src, staging)?;
            Ok(None)
        }
        InstallSource::Tarball(file) => {
            std::fs::create_dir_all(staging)
                .map_err(|err| format!("could not create {}: {err}", staging.display()))?;
            let mut cmd = std::process::Command::new("tar");
            cmd.arg("-xf").arg(file).arg("-C").arg(staging);
            run_tool(cmd, "tar extract")?;
            Ok(None)
        }
        InstallSource::Git { url, branch } => {
            let mut cmd = std::process::Command::new("git");
            cmd.args(["clone", "--depth", "1"]);
            if let Some(branch) = branch {
                cmd.args(["--branch", branch]);
            }
            cmd.arg(url).arg(staging);
            run_tool(cmd, "git clone")?;
            let mut head = std::process::Command::new("git");
            head.arg("-C").arg(staging).args(["rev-parse", "HEAD"]);
            let stdout = run_tool(head, "git rev-parse HEAD")?;
            // The managed copy is a snapshot, not a working clone
            // (ADR-0041): drop the checkout's .git so the installed tree
            // matches dir installs and cannot be mutated in place.
            let git_dir = staging.join(".git");
            std::fs::remove_dir_all(&git_dir)
                .map_err(|err| format!("could not remove {}: {err}", git_dir.display()))?;
            Ok(Some(stdout.trim().to_owned()))
        }
    }
}

/// Run one child process to completion, failing loudly with its stderr.
fn run_tool(mut cmd: std::process::Command, what: &str) -> Result<String, String> {
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
        if file_type.is_symlink() {
            return Err(format!(
                "{} is a symlink; plugin dir installs must be self-contained",
                from.display()
            ));
        }
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

fn load_fetched_manifest(plugin_root: &Path, reference: &str) -> Result<PluginManifest, String> {
    plugin::load_plugin_manifest(plugin_root)
        .map_err(|err| format!("fetched plugin from {reference} failed validation: {err}"))
}

/// Run the manifest's `[[build]]` steps that apply to this platform, each
/// bounded by [`BUILD_TIMEOUT`] with captured output.
fn run_build_steps(
    runtime: &tokio::runtime::Runtime,
    manifest: &PluginManifest,
) -> Result<(), String> {
    for (index, step) in manifest.build.iter().enumerate() {
        if !platform_matches(step.platforms.as_deref()) {
            continue;
        }
        let label = format!("build step {} ({})", index + 1, step.command.join(" "));
        let spec = CommandSpec {
            argv: step.command.clone(),
            cwd: Some(manifest.plugin_root.clone()),
            env: vec![
                ("PHUX_PLUGIN_ID".to_owned(), manifest.id.clone()),
                (
                    "PHUX_PLUGIN_ROOT".to_owned(),
                    manifest.plugin_root.display().to_string(),
                ),
            ],
            timeout: Some(BUILD_TIMEOUT),
        };
        let output = runtime
            .block_on(run_command_spec(spec))
            .map_err(|err| format!("{label} could not run: {err}"))?;
        match output.outcome {
            PluginActionOutcome::TimedOut => {
                return Err(format!(
                    "{label} timed out after {}s",
                    BUILD_TIMEOUT.as_secs()
                ));
            }
            PluginActionOutcome::Completed if output.exit_code != Some(0) => {
                return Err(format!(
                    "{label} failed with exit code {:?}\nstdout: {}\nstderr: {}",
                    output.exit_code,
                    output.stdout.trim(),
                    output.stderr.trim(),
                ));
            }
            PluginActionOutcome::Completed => {}
        }
    }
    Ok(())
}

fn platform_matches(platforms: Option<&[PluginPlatform]>) -> bool {
    match (platforms, CURRENT_PLATFORM) {
        (None, _) => true,
        (Some(list), Some(current)) => list.contains(&current),
        (Some(_), None) => false,
    }
}

/// Filesystem-safe directory name for a plugin id (ids allow `:` which
/// some filesystems reject).
fn install_dir_name(id: &str) -> String {
    id.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// Move the fetched plugin root into its final managed directory
/// (fresh install: the destination must not exist).
fn promote(staging: &Path, plugin_root: &Path, final_dir: &Path) -> Result<(), String> {
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
fn swap_install_dir(
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
struct StagingGuard {
    path: PathBuf,
    armed: bool,
}

impl StagingGuard {
    fn claim(plugins_dir: &Path) -> Self {
        let path = plugins_dir.join(format!(".staging-{}-{}", std::process::id(), temp_nonce()));
        Self { path, armed: true }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    const fn disarm(&mut self) {
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
