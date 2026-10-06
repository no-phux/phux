mod config_json;
mod live_feed;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_config::loader as config_loader;
use phux_config::plugin::{self, PluginManifest};
use phux_server::runtime::default_socket_path;

use super::config_action::ConfigAction;
use config_json::{print_agents_json, print_plugins_json};
use live_feed::{
    AgentProjection, LiveAgentFeed, ManifestAgentRow, ProjectionSource, fetch_live_feed,
    merge_agents,
};

/// `phux config <action>`: mostly client-local. `config agents` best-effort
/// reads live `phux.agent/v1` state, and `reload` rings the server-relayed
/// reload doorbell.
pub(crate) fn run_config(action: &ConfigAction, socket: Option<std::path::PathBuf>) -> ExitCode {
    match action {
        ConfigAction::Path => {
            outln!("{}", config_loader::config_path().display());
            ExitCode::SUCCESS
        }
        ConfigAction::Check { path, json } => run_config_check(path.as_deref(), *json),
        ConfigAction::Init { force, distro } => run_config_init(*force, distro.as_deref()),
        ConfigAction::Show {
            default,
            layers,
            json,
        } => run_config_show(*default, *layers, *json),
        ConfigAction::Plugins { json } => run_config_plugins(*json),
        ConfigAction::Agents { json } => run_config_agents(*json, socket),
        ConfigAction::Reload => run_config_reload(socket),
        ConfigAction::Run {
            plugin,
            action,
            timeout,
            cwd,
            json,
        } => run_config_action(plugin, action, *timeout, cwd.clone(), *json),
    }
}

/// `phux config check [PATH] [--json]`: every unknown key and wrong value in
/// the layer stack with its dotted path and layer, then a semantic pass for
/// chords that do not parse, unknown actions (with a suggestion), and shadowed
/// bindings. Exit 0 clean, 1 findings, 2 the check could not run.
fn run_config_check(path: Option<&Path>, json: bool) -> ExitCode {
    let path = path.map_or_else(config_loader::config_path, Path::to_path_buf);

    // A missing file is clean, not an error: no config means no overrides,
    // exactly as the loader treats it.
    let mut missing = false;
    let body = match std::fs::read_to_string(&path) {
        Ok(body) => body,
        // A missing file is clean, but say so rather than printing "ok" for
        // a path that does not exist — the operator may have checked the
        // wrong file, and a bare "ok" would hide that.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            missing = true;
            String::new()
        }
        Err(err) => {
            return check_unrunnable(json, &format!("cannot read {}: {err}", path.display()));
        }
    };

    let report = match phux_config::check::check(&body, &path) {
        Ok(report) => report,
        Err(err) => {
            return check_unrunnable(json, &err.to_string());
        }
    };

    if json {
        return print_check_json(&path, &report, missing);
    }
    print_check_human(&path, &report, missing)
}

/// Exit-2 "the check could not run" (unreadable file, malformed TOML):
/// prose without `--json` (unchanged spelling), the shared JSON error
/// contract with it (code `invalid_config`, exit 2 both in the document and
/// the process — phux-i0e8.8.3).
fn check_unrunnable(json: bool, message: &str) -> ExitCode {
    if json {
        return crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::INVALID_CONFIG,
                message,
                "fix the file at the reported path; `phux config path` names \
                 the active config",
            ),
            2,
        );
    }
    eprintln!("phux: {message}");
    ExitCode::from(2)
}

/// Human rendering: one line per finding, path first so the column scans.
fn print_check_human(path: &Path, report: &phux_config::CheckReport, missing: bool) -> ExitCode {
    if missing {
        outln!(
            "{}: no config file (shipped defaults apply)",
            path.display()
        );
        return ExitCode::SUCCESS;
    }
    if report.is_ok() {
        outln!("{}: ok", path.display());
        return ExitCode::SUCCESS;
    }

    for finding in &report.findings {
        outln!(
            "{}: {}: {}",
            finding.path,
            finding.fault.label(),
            finding.message
        );
        // Only worth a line when it is not the file the user just named;
        // repeating their own path on every finding is noise.
        let origin = finding.origin();
        if origin != path.display().to_string() {
            outln!("  from {origin}");
        }
    }

    let n = report.findings.len();
    let plural = if n == 1 { "problem" } else { "problems" };
    if report.truncated {
        outln!("{n} {plural} (list truncated; fix these and re-run)");
    } else {
        outln!("{n} {plural}");
    }
    ExitCode::FAILURE
}

/// JSON rendering for scripts and CI.
fn print_check_json(path: &Path, report: &phux_config::CheckReport, missing: bool) -> ExitCode {
    let findings: Vec<_> = report
        .findings
        .iter()
        .map(|finding| {
            serde_json::json!({
                "path": finding.path,
                "fault": finding.fault.label(),
                "message": finding.message,
                "origin": finding.origin(),
            })
        })
        .collect();
    let doc = serde_json::json!({
        "schema_version": 1,
        "config": path,
        "exists": !missing,
        "ok": report.is_ok(),
        "truncated": report.truncated,
        "findings": findings,
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(rendered) => {
            outln!("{rendered}");
            if report.is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        // A `--json` path, so the failure is the contract line, never prose.
        Err(err) => crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::JSON_SERIALIZE,
                format!("could not render check JSON: {err}"),
                "this is a phux bug; run `phux doctor` and report it",
            ),
            2,
        ),
    }
}

/// `phux config show [--default | --layers [--json]]`: `--default` echoes the
/// embedded defaults verbatim; plain `show` renders the merged document
/// (ADR-0039); `--layers` renders which layer set each key and array element.
fn run_config_show(default: bool, layers: bool, json: bool) -> ExitCode {
    if default {
        out!("{}", phux_config::DEFAULT_CONFIG_TOML);
        return ExitCode::SUCCESS;
    }
    let path = config_loader::config_path();
    let user_input = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            eprintln!("phux: could not read {}: {err}", path.display());
            return ExitCode::FAILURE;
        }
    };
    if layers {
        let provenance = match phux_config::merged_config_with_provenance(&user_input, &path) {
            Ok((_, provenance)) => provenance,
            Err(err) => {
                eprintln!("phux: {err}");
                return ExitCode::FAILURE;
            }
        };
        if json {
            return config_json::print_layers_json(&path, &provenance);
        }
        return print_layers_human(&provenance);
    }
    let merged = match phux_config::merged_config_table(&user_input, &path) {
        Ok(table) => table,
        Err(err) => {
            eprintln!("phux: {err}");
            return ExitCode::FAILURE;
        }
    };
    match toml::to_string(&merged) {
        Ok(rendered) => {
            out!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("phux: could not render config: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Render the provenance view: the layer stack, then one row per
/// effective leaf key (arrays expand to one row per element) tagged
/// with the 1-based index and short name of the owning layer.
fn print_layers_human(provenance: &phux_config::ConfigProvenance) -> ExitCode {
    outln!("layers (merge order; later layers win):");
    for (i, layer) in provenance.layers.iter().enumerate() {
        let label = match layer {
            phux_config::LayerSource::Defaults => "defaults (embedded)".to_owned(),
            phux_config::LayerSource::Extended(p) => p.display().to_string(),
            phux_config::LayerSource::User(p) => format!("{} (user)", p.display()),
        };
        outln!("  [{}] {label}", i + 1);
    }
    outln!();
    outln!("keys:");
    let mut rows: Vec<(String, usize)> = Vec::new();
    for (key, origin) in &provenance.keys {
        match origin.elements.as_deref() {
            Some(elements) if !elements.is_empty() => {
                for (i, layer) in elements.iter().enumerate() {
                    rows.push((format!("{key}[{i}]"), *layer));
                }
            }
            _ => rows.push((key.clone(), origin.layer)),
        }
    }
    let width = rows.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
    for (key, layer) in rows {
        let label = provenance
            .layers
            .get(layer)
            .map_or_else(|| "?".to_owned(), layer_short_label);
        outln!("  {key:<width$}  <- [{}] {label}", layer + 1);
    }
    ExitCode::SUCCESS
}

/// Short per-row tag for a layer: `defaults`, the layer file's name,
/// or `user`. The 1-based index printed beside it disambiguates layers
/// whose file names collide.
fn layer_short_label(layer: &phux_config::LayerSource) -> String {
    match layer {
        phux_config::LayerSource::Defaults => "defaults".to_owned(),
        phux_config::LayerSource::Extended(p) => p.file_name().map_or_else(
            || p.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
        phux_config::LayerSource::User(_) => "user".to_owned(),
    }
}

/// The source checkout's `distros/`, the last place a bundled distro name is
/// looked up (absent on installed builds). The `phux` binary records it at
/// startup; see [`crate::set_checkout_distros`].
pub(crate) static CHECKOUT_DISTROS: std::sync::OnceLock<&'static Path> = std::sync::OnceLock::new();

/// `phux config init [--distro <name-or-path>]`: scaffold the starter config,
/// validating the full merged stack before writing anything.
fn run_config_init(force: bool, distro: Option<&str>) -> ExitCode {
    let path = config_loader::config_path();
    let contents = match distro {
        None => phux_config::scaffold::reference_config(),
        Some(spec) => {
            let checkout = CHECKOUT_DISTROS.get().copied();
            let layer = match phux_config::distro::resolve_distro(spec, checkout) {
                Ok(layer) => layer,
                Err(err) => {
                    eprintln!("phux: {err}");
                    return ExitCode::FAILURE;
                }
            };
            let contents = phux_config::scaffold::distro_reference_config(&layer);
            if let Err(err) = phux_config::parse_with_defaults(&contents, &path) {
                eprintln!(
                    "phux: distro layer {} does not produce a valid config: {err}",
                    layer.display()
                );
                return ExitCode::FAILURE;
            }
            contents
        }
    };
    match phux_config::scaffold::write_scaffold(&path, &contents, force) {
        Ok(phux_config::scaffold::ScaffoldOutcome::Wrote(p)) => {
            outln!("wrote {}", p.display());
            ExitCode::SUCCESS
        }
        Ok(phux_config::scaffold::ScaffoldOutcome::Skipped(p)) => {
            eprintln!(
                "phux: {} already exists; refusing to overwrite (use --force)",
                p.display()
            );
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("phux: could not write config: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Why an attached TUI would refuse to reload `path`, judged by the same
/// strict build it runs (layered loader, status-bar widgets, and every
/// binding), so the CLI never reports "config OK" for a file the clients
/// then reject.
fn reload_refusal(path: &Path) -> Option<String> {
    phux_tui::settings::TuiSettings::load_strict(path).err()
}

/// `phux config reload`: validate the config locally (a broken file fails
/// here and signals nothing), then ring the `phux.config.reload/v1` doorbell
/// with a fresh nonce; each client re-reads its own file.
fn run_config_reload(socket: Option<PathBuf>) -> ExitCode {
    // 1. Validate locally with the build the clients run on reload.
    let config_path = config_loader::config_path();
    if let Some(err) = reload_refusal(&config_path) {
        eprintln!("phux: config invalid, not signalling reload: {err}");
        eprintln!("  run `phux config check` to list every problem");
        return ExitCode::FAILURE;
    }

    // 2. Ring the doorbell.
    let socket_path = socket.unwrap_or_else(phux_server::runtime::default_socket_path);
    let rt = match super::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    match rt.block_on(ring_config_reload(&socket_path)) {
        Ok(()) => {
            outln!("config OK; reload signalled to attached clients and a running hub");
            ExitCode::SUCCESS
        }
        Err(ReloadRingError::NoServer(err)) => {
            super::report_no_server(&err, &socket_path, "config reload")
        }
        Err(ReloadRingError::Unconfirmed(refusal)) => {
            eprintln!("phux: config reload could not be confirmed: {refusal}");
            ExitCode::FAILURE
        }
    }
}

/// Why [`ring_config_reload`] did not confirm the doorbell.
#[derive(Debug)]
pub(crate) enum ReloadRingError {
    /// No server answered at the socket.
    NoServer(phux_client::attach::AttachError),
    /// The server answered the read-back with a refusal.
    Unconfirmed(String),
}

/// Ring the `phux.config.reload/v1` doorbell on the server at `socket_path`
/// with a fresh nonce and wait for the read-back. The server handles a
/// connection's frames in order, so `Ok` also means a hub has already
/// re-read its `[[satellites]]` (L3 §3.8).
pub(crate) async fn ring_config_reload(socket_path: &Path) -> Result<(), ReloadRingError> {
    use phux_client::attach::connection::Connection;
    use phux_protocol::wire::frame::{CONFIG_RELOAD_KEY, FrameKind, Scope};

    let mut conn = Connection::connect(socket_path)
        .await
        .map_err(ReloadRingError::NoServer)?;
    // The nonce only has to differ from the previous value (the server
    // dedups equal-bytes SETs).
    let nonce = format!(
        "{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
        std::process::id(),
    );
    conn.send(&FrameKind::SetMetadata {
        request_id: 1,
        scope: Scope::Global,
        key: CONFIG_RELOAD_KEY.to_owned(),
        value: nonce.into_bytes(),
    })
    .await
    .map_err(ReloadRingError::NoServer)?;
    // Read-back as a flush barrier (`SET_METADATA` has no reply), via
    // `request_metadata` so a correlated ERROR cannot hang it.
    let reply = conn
        .request_metadata(2, Scope::Global, CONFIG_RELOAD_KEY.to_owned())
        .await
        .map_err(ReloadRingError::NoServer)?;
    drop(conn);
    // `handle_get_metadata` (`crates/phux-server/src/runtime/client.rs`)
    // answers with METADATA_VALUE and pushes nothing of its own, and this
    // connection is a fresh one-shot that never attached or subscribed —
    // nothing can fan out onto it. A non-empty discard is logged.
    reply
        .into_result_ignoring_interleaved()
        .map(drop)
        .map_err(|refusal| ReloadRingError::Unconfirmed(refusal.to_string()))
}

pub(super) struct LoadedPlugin {
    pub(super) enabled: bool,
    pub(super) manifest: PluginManifest,
}

fn load_configured_plugins() -> Result<Vec<LoadedPlugin>, String> {
    let path = config_loader::config_path();
    let cfg = match config_loader::load_from(&path) {
        Ok(cfg) => cfg,
        Err(err) => return Err(err.to_string()),
    };
    let mut loaded = Vec::new();
    for entry in cfg.plugins {
        let manifest_path = plugin::resolve_manifest_path(&entry.manifest, &path);
        let manifest = match plugin::load_plugin_manifest(&manifest_path) {
            Ok(manifest) => manifest,
            Err(err) => {
                return Err(format!("could not load {}: {err}", manifest_path.display()));
            }
        };
        loaded.push(LoadedPlugin {
            enabled: entry.enabled,
            manifest,
        });
    }
    Ok(loaded)
}

fn run_config_plugins(json: bool) -> ExitCode {
    let loaded = match load_configured_plugins() {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("phux: {err}");
            return ExitCode::FAILURE;
        }
    };
    if json {
        return print_plugins_json(&loaded);
    }
    for plugin in loaded {
        let state = if plugin.enabled {
            "enabled"
        } else {
            "disabled"
        };
        let manifest = plugin.manifest;
        outln!("{} {} ({state})", manifest.id, manifest.version);
    }
    ExitCode::SUCCESS
}

fn run_config_agents(json: bool, socket: Option<PathBuf>) -> ExitCode {
    let loaded = match load_configured_plugins() {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("phux: {err}");
            return ExitCode::FAILURE;
        }
    };
    let rows = manifest_agent_rows(&loaded);
    // phux-r82.10: best-effort live feed. No server (or any transport
    // failure) means `feed` is `None` and the projection reports the
    // declared manifest values, exactly as before.
    let feed = fetch_feed_blocking(socket);
    let merged = merge_agents(&rows, feed.as_ref());
    if json {
        return print_agents_json(&merged, feed.is_some());
    }
    for row in &merged {
        let plugin_state = if row.plugin_enabled {
            "enabled"
        } else {
            "disabled"
        };
        outln!(
            "{}:{} {} {} {} ({plugin_state}, {})",
            row.plugin_id,
            row.id,
            row.label,
            row.state,
            row.attention,
            provenance_word(row)
        );
    }
    ExitCode::SUCCESS
}

/// Flatten the loaded manifests into the merge input rows.
fn manifest_agent_rows(loaded: &[LoadedPlugin]) -> Vec<ManifestAgentRow> {
    loaded
        .iter()
        .flat_map(|plugin| {
            plugin.manifest.agents.iter().map(|agent| ManifestAgentRow {
                plugin_id: plugin.manifest.id.clone(),
                plugin_enabled: plugin.enabled,
                agent: agent.clone(),
            })
        })
        .collect()
}

/// Fetch the live feed on a throwaway current-thread runtime; `None` when
/// no runtime can be built or no server answers.
fn fetch_feed_blocking(socket: Option<PathBuf>) -> Option<LiveAgentFeed> {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = crate::commands::cli_runtime().ok()?;
    rt.block_on(fetch_live_feed(&socket_path))
}

/// Human-output provenance suffix: where the effective state came from.
fn provenance_word(row: &AgentProjection) -> String {
    match (&row.source, &row.runtime) {
        (ProjectionSource::Runtime, Some(binding)) => format!("live {}", binding.terminal),
        _ => "declared".to_owned(),
    }
}

fn run_config_action(
    plugin: &str,
    action: &str,
    timeout: Option<u64>,
    cwd: Option<PathBuf>,
    json: bool,
) -> ExitCode {
    let path = config_loader::config_path();
    let timeout = timeout.map(Duration::from_secs);
    let request = phux_plugin::PluginActionRequest {
        plugin_id: plugin.to_owned(),
        action_id: action.to_owned(),
        timeout,
        cwd,
    };
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    match rt.block_on(phux_plugin::run_configured_action_logged(&path, &request)) {
        Ok(output) => print_action_output(&output, json),
        Err(err) => {
            eprintln!("phux: {err}");
            ExitCode::FAILURE
        }
    }
}

fn print_action_output(output: &phux_plugin::PluginActionOutput, json: bool) -> ExitCode {
    if json {
        return match serde_json::to_string_pretty(output) {
            Ok(rendered) => {
                outln!("{rendered}");
                action_exit_code(output)
            }
            Err(err) => {
                eprintln!("phux: could not render plugin action JSON: {err}");
                ExitCode::FAILURE
            }
        };
    }
    out!("{}", output.stdout);
    eprint!("{}", output.stderr);
    action_exit_code(output)
}

fn action_exit_code(output: &phux_plugin::PluginActionOutput) -> ExitCode {
    match output.outcome {
        phux_plugin::PluginActionOutcome::Completed => output
            .exit_code
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from),
        phux_plugin::PluginActionOutcome::TimedOut => {
            // `run`'s timeout convention (canonical table: exit_codes.rs).
            ExitCode::from(crate::exit_codes::EXIT_RUN_TIMEOUT)
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    /// `config reload` judges the file the way the attached clients will: a
    /// binding to a misspelled action parses fine but is refused on reload,
    /// so the CLI must refuse it too instead of printing "config OK".
    #[test]
    fn reload_refusal_matches_the_clients_strict_build() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");

        std::fs::write(&path, "[keybindings.prefix-table]\nc = \"new-windoww\"\n")
            .expect("write config");
        let refusal = super::reload_refusal(&path).expect("unknown action refused");
        assert!(
            refusal.contains("unknown action `new-windoww`"),
            "{refusal}"
        );

        std::fs::write(&path, "[keybindings.prefix-table]\nc = \"new-window\"\n")
            .expect("write config");
        assert_eq!(super::reload_refusal(&path), None);
    }
}
