//! `phux plugin install` / `phux plugin update` (phux-r82.8).
//!
//! Installs are source-agnostic fetches into one managed directory:
//! GitHub `owner/repo[/subdir]` shorthand, a git URL (cloned with the
//! system `git`), a local plugin directory (copied), or a local tarball
//! (extracted with the system `tar`) lands under the phux data dir
//! (`$XDG_DATA_HOME/phux/plugins`, else `~/.local/share/phux/plugins`).
//! The fetched manifest's `[[build]]` steps for this platform then run as
//! child processes with a bounded timeout and captured output (the
//! phux-plugin runtime contract), the manifest is validated — including
//! the `min_phux_version` gate — and the package is linked into
//! `config.toml` through the same registry path as `phux plugin link`.
//! Provenance (ref, branch, subdirectory, resolved commit) is recorded in
//! the managed dir's `plugins.lock` so `phux plugin update` can re-fetch
//! without re-asking the user.
//!
//! Source choice lives in [`source`]; fetching lives in [`fetch`].

mod confirm;
mod fetch;
mod git;
mod source;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_config::plugin::{self, PluginManifest, PluginPlatform};
use phux_plugin::{CommandSpec, PluginActionOutcome, run_command_spec};

use super::lock::{
    self, PluginLockEntry, lockfile_path, read_lockfile, upsert_entry, write_lockfile,
};
use super::{fail, upsert_config_entry};
use confirm::{build_step_lines, confirm_remote_build};
use fetch::{
    PinCheck, StagingGuard, fetch_into_staging, locate_fetched_root, promote, seal_git_checkout,
    swap_install_dir,
};
use git::is_commit_id;
use source::{InstallSource, classify_source, source_from_lock};

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

/// `phux plugin install REF [--rev REV] [--subdir PATH] [--disabled] [--json]`.
pub(super) fn run_install(
    reference: &str,
    rev: Option<&str>,
    subdir: Option<&str>,
    disabled: bool,
    yes: bool,
    json: bool,
) -> ExitCode {
    let runtime = match crate::commands::cli_runtime() {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    match install(&runtime, reference, rev, subdir, !disabled, yes, json) {
        Ok(code) => code,
        Err(err) => fail(json, &err),
    }
}

/// `phux plugin update [NAME] [--latest] [--yes] [--json]`.
pub(super) fn run_update(name: Option<&str>, latest: bool, yes: bool, json: bool) -> ExitCode {
    let runtime = match crate::commands::cli_runtime() {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    match update(&runtime, name, latest, yes, json) {
        Ok(code) => code,
        Err(err) => fail(json, &err),
    }
}

fn install(
    runtime: &tokio::runtime::Runtime,
    reference: &str,
    rev: Option<&str>,
    subdir: Option<&str>,
    enabled: bool,
    yes: bool,
    json: bool,
) -> Result<ExitCode, String> {
    let classified = classify_source(reference, rev, subdir)?;
    let source = &classified.source;
    let plugins_dir = plugins_data_dir()?;
    std::fs::create_dir_all(&plugins_dir)
        .map_err(|err| format!("could not create {}: {err}", plugins_dir.display()))?;

    let mut staged = stage_plugin(runtime, source, &plugins_dir, reference, yes)?;
    let final_dir = plugins_dir.join(install_dir_name(&staged.manifest.id));
    promote_staged_plugin(&mut staged, &final_dir)?;

    // Reload from the managed location so the linked manifest path (and
    // the plugin root actions execute from) is the installed copy.
    let manifest = load_fetched_manifest(&final_dir, reference)?;

    record_install_in_lockfile(
        &plugins_dir,
        source,
        &manifest.id,
        staged.resolved_rev.as_deref(),
        lock::hash_build_steps(&build_step_lines(&manifest.build)),
    )?;

    let entry = upsert_config_entry(manifest, enabled)?;
    Ok(report_installed(
        &entry,
        &final_dir,
        source,
        staged.resolved_rev.as_deref(),
        classified.note.as_deref(),
        json,
    ))
}

/// A plugin fetched into the staging directory and built, but not yet moved
/// into its managed location.
struct StagedPlugin {
    /// Removes the staging directory unless the promotion disarms it.
    staging: StagingGuard,
    /// The plugin root inside the staging directory.
    root: PathBuf,
    /// The manifest as fetched, before the reload from the managed location.
    manifest: PluginManifest,
    /// The commit the fetch resolved to, when the source names one.
    resolved_rev: Option<String>,
}

/// Fetch the source into staging, validate its manifest, and run its
/// `[[build]]` steps — everything that happens before anything under the
/// managed plugins directory is touched.
fn stage_plugin(
    runtime: &tokio::runtime::Runtime,
    source: &InstallSource,
    plugins_dir: &Path,
    reference: &str,
    yes: bool,
) -> Result<StagedPlugin, String> {
    // Build steps live in the fetched manifest, so the clone happens first.
    // A refusal drops this staging directory and installs nothing.
    let fetched = fetch_tree(source, plugins_dir, reference, None)?;
    if matches!(source, InstallSource::Git { .. }) {
        confirm_remote_build(&fetched.manifest, None, yes)?;
    }
    run_build_steps(runtime, &fetched.manifest)?;
    Ok(fetched)
}

fn fetch_tree(
    source: &InstallSource,
    plugins_dir: &Path,
    reference: &str,
    pin: Option<PinCheck<'_>>,
) -> Result<StagedPlugin, String> {
    let staging = StagingGuard::claim(plugins_dir);
    let resolved_rev = fetch_into_staging(source, staging.path())?;
    let root = locate_fetched_root(staging.path(), source.subdir())?;
    if matches!(source, InstallSource::Git { .. }) {
        let rev = resolved_rev
            .as_deref()
            .ok_or_else(|| "git fetch did not resolve a commit".to_owned())?;
        seal_git_checkout(staging.path(), &root, rev, pin)?;
    }
    let manifest = load_fetched_manifest(&root, reference)?;
    Ok(StagedPlugin {
        staging,
        root,
        manifest,
        resolved_rev,
    })
}

/// Move the staged plugin into its managed location, refusing to install over
/// a plugin that is already there — `phux plugin update` is the way to refresh
/// one, so an install can never clobber an existing tree.
fn promote_staged_plugin(staged: &mut StagedPlugin, final_dir: &Path) -> Result<(), String> {
    if final_dir.exists() {
        return Err(format!(
            "plugin {} is already installed at {}; run `phux plugin update {}` to refresh it",
            staged.manifest.id,
            final_dir.display(),
            staged.manifest.id,
        ));
    }
    promote(staged.staging.path(), &staged.root, final_dir)?;
    staged.staging.disarm();
    Ok(())
}

/// Record the install's provenance (ref, branch, subdirectory, resolved
/// commit) in the managed dir's `plugins.lock`, so `phux plugin update` can
/// re-fetch without re-asking the user.
fn record_install_in_lockfile(
    plugins_dir: &Path,
    source: &source::InstallSource,
    id: &str,
    resolved_rev: Option<&str>,
    build_steps: Option<String>,
) -> Result<(), String> {
    let mut lockfile = read_lockfile(plugins_dir)?;
    upsert_entry(
        &mut lockfile,
        PluginLockEntry {
            id: id.to_owned(),
            source: source.kind(),
            source_ref: source.source_ref(),
            branch: source.branch(),
            subdir: source.subdir().map(str::to_owned),
            rev: resolved_rev.map(str::to_owned),
            build_steps,
        },
    );
    write_lockfile(plugins_dir, &lockfile)
}

/// Report the finished install, as the `--json` document or the human line.
fn report_installed(
    entry: &super::registry::RegistryEntry,
    final_dir: &Path,
    source: &source::InstallSource,
    resolved_rev: Option<&str>,
    note: Option<&str>,
    json: bool,
) -> ExitCode {
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
                "subdir": source.subdir(),
                "rev": resolved_rev,
                "note": note,
                "enabled": entry.enabled,
            },
        });
        return crate::output::json(&doc);
    }
    if let Some(note) = note {
        info!("{note}");
    }
    let state = if entry.enabled { "enabled" } else { "disabled" };
    let rev_note = resolved_rev.map_or_else(String::new, |rev| format!(" at {rev}"));
    outln!(
        "installed {} {}{rev_note} -> {} ({state})",
        entry.manifest.id,
        entry.manifest.version,
        final_dir.display(),
    );
    ExitCode::SUCCESS
}

/// One `phux plugin update` result line.
struct UpdateReport {
    id: String,
    version: String,
    rev: Option<String>,
    branch: Option<String>,
    build_steps: Option<String>,
}

fn update(
    runtime: &tokio::runtime::Runtime,
    name: Option<&str>,
    latest: bool,
    yes: bool,
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
        let report = update_one(runtime, &plugins_dir, &entry, latest, yes)?;
        lockfile.plugins[index].rev.clone_from(&report.rev);
        lockfile.plugins[index].branch.clone_from(&report.branch);
        lockfile.plugins[index]
            .build_steps
            .clone_from(&report.build_steps);
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
        return Ok(crate::output::json(&doc));
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
    latest: bool,
    yes: bool,
) -> Result<UpdateReport, String> {
    let pinned_sha = entry.branch.as_deref().is_some_and(is_commit_id);
    let mut source = source_from_lock(entry)?;
    if pinned_sha && let InstallSource::Git { branch, .. } = &mut source {
        if latest {
            *branch = None;
        } else if let Some(full) = entry.rev.clone().filter(|rev| git::is_full_commit(rev)) {
            *branch = Some(full);
        }
    }
    let pin = entry.rev.as_deref().map(|previous| PinCheck {
        previous,
        branch: entry
            .branch
            .as_deref()
            .filter(|branch| !is_commit_id(branch)),
        pinned_sha,
        latest,
    });
    let fetched = fetch_tree(&source, plugins_dir, &entry.source_ref, pin)?;
    if fetched.manifest.id != entry.id {
        return Err(format!(
            "source {} now provides plugin id {:?} (expected {:?}); refusing to update",
            entry.source_ref, fetched.manifest.id, entry.id,
        ));
    }
    // Same as install: the new steps are known only after the fetch. A
    // refusal drops staging and leaves the installed copy where it was.
    if matches!(source, InstallSource::Git { .. }) {
        confirm_remote_build(&fetched.manifest, entry.build_steps.as_deref(), yes)?;
    }
    run_build_steps(runtime, &fetched.manifest)?;
    let final_dir = plugins_dir.join(install_dir_name(&entry.id));
    swap_install_dir(
        plugins_dir,
        fetched.staging.path(),
        &fetched.root,
        &final_dir,
    )?;
    let stored_branch = if pinned_sha && latest {
        None
    } else {
        source.branch()
    };
    Ok(UpdateReport {
        id: entry.id.clone(),
        version: fetched.manifest.version,
        rev: fetched.resolved_rev,
        branch: stored_branch,
        build_steps: lock::hash_build_steps(&build_step_lines(&fetched.manifest.build)),
    })
}

/// The managed plugins directory under the phux data dir:
/// `$XDG_DATA_HOME/phux/plugins`, else `~/.local/share/phux/plugins`.
/// Refused to a development build when it is the production directory (it
/// is not profile-scoped, and the production server runs what lands there).
fn plugins_data_dir() -> Result<PathBuf, String> {
    let dir = unguarded_plugins_data_dir()?;
    phux_config::production::refuse_dev_on_production_state(&dir)?;
    Ok(dir)
}

fn unguarded_plugins_data_dir() -> Result<PathBuf, String> {
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
