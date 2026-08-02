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

/// `phux config <action>` (phux-ijp). Mostly client-local: inspects and
/// scaffolds the on-disk config without contacting a server. The
/// exceptions are `config agents`, which best-effort reads live
/// `phux.agent/v1` state from a running server (phux-r82.10) and degrades
/// to the declared manifest values when none answers, and `reload`
/// (phux-foz.5), which rings
/// the server-relayed reload doorbell for attached clients.
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

/// `phux config check [PATH] [--json]` (phux-q9wj.3).
///
/// Reports every unknown key and wrong value in the resolved layer stack,
/// each with its full dotted path and the layer file that introduced it.
/// Once the stack deserializes, a semantic pass (phux-i0e8.3.2) also
/// reports keybinding chords that do not parse, action names no dispatcher
/// arm handles (with a did-you-mean suggestion), and bindings that shadow
/// each other in the resolver — the mistakes that load fine and then
/// silently do nothing.
///
/// Exit codes: 0 clean, 1 findings, 2 the check could not run (unreadable
/// file, malformed TOML, cyclic `extends`). The three are distinct because a
/// CI gate wants to fail differently on "your config has a typo" than on "I
/// could not read your config at all".
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
            eprintln!("phux: cannot read {}: {err}", path.display());
            return ExitCode::from(2);
        }
    };

    let report = match phux_config::check::check(&body, &path) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("phux: {err}");
            return ExitCode::from(2);
        }
    };

    if json {
        return print_check_json(&path, &report, missing);
    }
    print_check_human(&path, &report, missing)
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
        Err(err) => {
            eprintln!("phux: could not render check JSON: {err}");
            ExitCode::from(2)
        }
    }
}

/// `phux config show [--default | --layers [--json]]`.
///
/// `--default` echoes the embedded defaults verbatim, comments and all
/// — the annotated source of truth. Plain `show` renders the effective
/// merged document (defaults + `extends` layers + the user's overrides,
/// ADR-0039) as canonical TOML. `--layers` renders provenance instead:
/// which layer of the stack set each effective key, and for `-append`
/// arrays, which layer contributed each element.
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

/// `phux config init [--distro <name-or-path>]`: scaffold the starter
/// config, plain or extending a starter distribution (phux-r82.9).
///
/// The distro flavor resolves the spec to an absolute layer path, then
/// **validates the full merged stack before writing anything** — a
/// broken or missing distro layer fails the command instead of leaving
/// the user a config that errors on every subsequent invocation.
fn run_config_init(force: bool, distro: Option<&str>) -> ExitCode {
    let path = config_loader::config_path();
    let contents = match distro {
        None => phux_config::scaffold::reference_config(),
        Some(spec) => {
            let layer = match phux_config::distro::resolve_distro(spec) {
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

/// `phux config reload` (phux-foz.5): validate the layered config
/// locally, then ring the `phux.config.reload/v1` doorbell so attached
/// clients re-read their config in place.
///
/// The local validation is the config-iteration fast path: a broken file
/// fails HERE, with the parse error on stderr, and nothing is signalled
/// (running clients would have kept their old config anyway — the
/// doorbell just becomes pointless noise). The signal is a `SET_METADATA`
/// of the conventional Global key with a fresh nonce value; the config
/// bytes never cross the wire — every client re-reads its own file. The
/// trailing `GET_METADATA` round-trip is load-bearing: `SET_METADATA` has
/// no reply frame, so without it this process could exit and close the
/// socket before the server reads the SET (same pattern as `phux tag`).
fn run_config_reload(socket: Option<PathBuf>) -> ExitCode {
    use phux_client::attach::connection::Connection;
    use phux_protocol::wire::frame::{CONFIG_RELOAD_KEY, FrameKind, Scope};

    // 1. Validate locally with the full layered loader (extends stacks).
    let config_path = config_loader::config_path();
    if let Err(err) = config_loader::load_from(&config_path) {
        eprintln!("phux: config invalid, not signalling reload: {err}");
        return ExitCode::FAILURE;
    }

    // 2. Ring the doorbell. The nonce only has to differ from the
    // previous value (the server dedups equal-bytes SETs).
    let socket_path = socket.unwrap_or_else(phux_server::runtime::default_socket_path);
    let rt = match super::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let mut conn = match Connection::connect(&socket_path).await {
            Ok(conn) => conn,
            Err(err) => return super::report_no_server(&err, &socket_path, "config reload"),
        };
        let nonce = format!(
            "{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
            std::process::id(),
        );
        if let Err(err) = conn
            .send(&FrameKind::SetMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: CONFIG_RELOAD_KEY.to_owned(),
                value: nonce.into_bytes(),
            })
            .await
        {
            return super::report_no_server(&err, &socket_path, "config reload");
        }
        // The read-back is a flush barrier: SET_METADATA has no reply, so
        // without it this process can exit and close the socket before the
        // server reads the SET. It goes through `request_metadata` because the
        // wait that used to be here matched METADATA_VALUE alone — a server
        // that refused the read with a correlated ERROR (`proto.md` §9) left
        // `phux config reload` hanging with no output at all.
        let reply = match conn
            .request_metadata(2, Scope::Global, CONFIG_RELOAD_KEY.to_owned())
            .await
        {
            Ok(reply) => reply,
            Err(err) => return super::report_no_server(&err, &socket_path, "config reload"),
        };
        // `handle_get_metadata` (`crates/phux-server/src/runtime/client.rs`)
        // answers with METADATA_VALUE and pushes nothing of its own, and this
        // connection is a fresh one-shot that never attached or subscribed —
        // nothing can fan out onto it. A non-empty discard is logged.
        if let Err(refusal) = reply.into_result_ignoring_interleaved() {
            eprintln!("phux: config reload could not be confirmed: {refusal}");
            return ExitCode::FAILURE;
        }
        outln!("config OK; reload signalled to attached clients");
        ExitCode::SUCCESS
    })
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
    match rt.block_on(phux_plugin::run_configured_action(&path, &request)) {
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
        phux_plugin::PluginActionOutcome::TimedOut => ExitCode::from(125),
    }
}
