//! Versioned, isolated native SDK runtime; provider packages are never shipped
//! inside phux or installed over the user's existing agent executables.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::Deserialize;
use sha2::{Digest, Sha256};

const MANIFEST: &str = include_str!("../../../../../integrations/native-host/package.json");
const LOCK: &str = include_str!("../../../../../integrations/native-host/package-lock.json");
const POLICY: &str = include_str!("../../../../../integrations/native-host/runtime-policy.json");
const SETUP: &str = include_str!("../../../../../integrations/native-host/scripts/setup.mjs");
const ASSETS: &str = include_str!("../../../../../integrations/native-host/scripts/assets.mjs");
const BRIDGE: &str = include_str!("../../../../../integrations/native-host/index.js");

#[derive(Deserialize)]
struct Ready {
    schema: String,
    platform: String,
    lock: String,
}

fn data_root() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("XDG_DATA_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".local/share"))
        .ok_or_else(|| "HOME or XDG_DATA_HOME is required for isolated SDK runtime".to_owned())
}

fn digest(bytes: &[u8]) -> String {
    crate::commands::update::apply::hex_lower(&Sha256::digest(bytes))
}

fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// Stable directory for this exact bridge and dependency graph.
pub(super) fn path() -> Result<PathBuf, String> {
    let mut hash = Sha256::new();
    for source in [MANIFEST, LOCK, POLICY, BRIDGE] {
        hash.update(source.as_bytes());
    }
    Ok(data_root()?
        .join("phux/native-sdk")
        .join(crate::commands::update::apply::hex_lower(&hash.finalize())))
}

/// Existing SDK assets only; discovery never installs or accepts terms.
pub(crate) fn available() -> Result<PathBuf, String> {
    let root = path()?;
    let marker = std::fs::read(root.join(".ready")).map_err(|_| setup_remedy())?;
    let ready: Ready = serde_json::from_slice(&marker)
        .map_err(|error| format!("invalid native SDK runtime marker: {error}"))?;
    if ready.schema != "phux.native-sdk-runtime/v1"
        || ready.platform != platform()
        || ready.lock != digest(LOCK.as_bytes())
        || !root.join("index.mjs").is_file()
    {
        return Err(format!(
            "native SDK runtime {} does not match this phux build; move the invalid cache aside, then run `phux agent sdk-setup`",
            root.display()
        ));
    }
    Ok(root)
}

fn setup_remedy() -> String {
    "Native Node SDK runtime is not prepared. Run `phux agent sdk-setup` on the executing host; it explains the required commercial terms and explicit acceptance flags. OMP/ACP Rust adapters do not need this runtime.".to_owned()
}

/// Resolve the actual executing-host Node executable, not a frontend path.
pub(crate) fn node_executable() -> Result<PathBuf, String> {
    let node = phux_plugin::executable_path("node").ok_or_else(|| {
        "Native SDK runtime requires Node >=22.19 on the executing host".to_owned()
    })?;
    let output = Command::new(&node)
        .args(["-p", "JSON.stringify(process.versions.node)"])
        .output()
        .map_err(|error| format!("could not inspect Node: {error}"))?;
    if !output.status.success() {
        return Err("Node version query failed".to_owned());
    }
    let version: String = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Node returned invalid version metadata: {error}"))?;
    let mut parts = version.split('.');
    let major = parts.next().and_then(|value| value.parse::<u32>().ok());
    let minor = parts.next().and_then(|value| value.parse::<u32>().ok());
    if !matches!((major, minor), (Some(major), Some(minor)) if major > 22 || major == 22 && minor >= 19)
    {
        return Err(format!(
            "Node {version} is too old; native SDK runtime requires >=22.19"
        ));
    }
    Ok(node)
}

/// Prepare isolated dependencies only after exact commercial terms consent.
pub(super) fn setup(accept_claude_terms: bool, accept_amp_terms: bool, json: bool) -> ExitCode {
    if !accept_claude_terms || !accept_amp_terms {
        return crate::commands::json_err::emit(
            json,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::USAGE_ERROR,
                "Native SDK setup requires explicit Claude and Amp commercial-term acceptance.",
                "Review https://www.anthropic.com/legal/commercial-terms and https://ampcode.com/terms. Pass --accept-claude-terms --accept-amp-terms only if authorized; dependencies stay in the private runtime, not PATH.",
            ),
            crate::exit_codes::EXIT_USAGE,
        );
    }
    let result = prepare();
    match result {
        Ok(root) => {
            if json {
                crate::output::json(
                    &serde_json::json!({"schema_version":1,"runtime":root,"isolated":true}),
                )
            } else {
                outln!("Native SDK runtime prepared: {}", root.display());
                ExitCode::SUCCESS
            }
        }
        Err(message) => crate::commands::json_err::emit(
            json,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::SDK_RUNTIME,
                format!("native SDK setup failed: {message}"),
                "Check the executing host's Node version, reviewed runtime policy, and private cache permissions; no provider session was started.",
            ),
            crate::exit_codes::EXIT_FAILURE,
        ),
    }
}

fn prepare() -> Result<PathBuf, String> {
    if let Ok(root) = available() {
        return Ok(root);
    }
    let node = node_executable()?;
    let root = path()?;
    let parent = root.parent().ok_or("native runtime path has no parent")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let artifacts = parent.join(format!(".setup-{}", std::process::id()));
    std::fs::create_dir(&artifacts).map_err(|error| error.to_string())?;
    let result = run_setup(&node, &root, &artifacts);
    let cleanup = std::fs::remove_dir_all(&artifacts);
    match (result, cleanup) {
        (Ok(()), Ok(())) => available(),
        (Err(error), _) => Err(error),
        (_, Err(error)) => Err(format!("could not remove owned setup artifacts: {error}")),
    }
}

fn run_setup(node: &Path, root: &Path, artifacts: &Path) -> Result<(), String> {
    let sources = [
        ("package.json", MANIFEST),
        ("package-lock.json", LOCK),
        ("runtime-policy.json", POLICY),
        ("index.mjs", BRIDGE),
        ("setup.mjs", SETUP),
        ("assets.mjs", ASSETS),
    ];
    for (name, source) in sources {
        std::fs::write(artifacts.join(name), source).map_err(|error| error.to_string())?;
    }
    let output = Command::new(node)
        .arg(artifacts.join("setup.mjs"))
        .arg(root)
        .arg(artifacts.join("package.json"))
        .arg(artifacts.join("package-lock.json"))
        .arg(artifacts.join("runtime-policy.json"))
        .arg(artifacts.join("index.mjs"))
        .args(["--accept-claude-terms", "--accept-amp-terms"])
        .output()
        .map_err(|error| format!("SDK setup process: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "SDK setup process exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    // Success diagnostics may accompany the final stdout document. Failures
    // retain these diagnostics inside the one structured CLI error instead.
    std::io::Write::write_all(&mut std::io::stderr().lock(), &output.stderr)
        .map_err(|error| format!("SDK setup diagnostics: {error}"))?;
    Ok(())
}
