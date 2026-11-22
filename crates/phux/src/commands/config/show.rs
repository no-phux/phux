//! `phux config show`: the merged config, the defaults, or which layer set each key.

use std::process::ExitCode;

use phux_config::loader as config_loader;

use super::config_json;

/// `phux config show [--default | --layers [--json]]`: `--default` echoes the
/// embedded defaults verbatim; plain `show` renders the merged document
/// (ADR-0039); `--layers` renders which layer set each key and array element.
pub(super) fn run_config_show(default: bool, layers: bool, json: bool) -> ExitCode {
    if default {
        out!("{}", phux_config::DEFAULT_CONFIG_TOML);
        return ExitCode::SUCCESS;
    }
    let path = config_loader::config_path();
    let user_input = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            error!("could not read {}: {err}", path.display());
            return ExitCode::FAILURE;
        }
    };
    if layers {
        let provenance = match phux_config::merged_config_with_provenance(&user_input, &path) {
            Ok((_, provenance)) => provenance,
            Err(err) => {
                error!("{err}");
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
            error!("{err}");
            return ExitCode::FAILURE;
        }
    };
    match toml::to_string(&merged) {
        Ok(rendered) => {
            out!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            error!("could not render config: {err}");
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
