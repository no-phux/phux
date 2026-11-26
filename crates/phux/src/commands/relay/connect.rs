//! `phux relay connect HOST:PORT`: the server side of a relay route. The
//! token `phux relay pair` printed goes into an owner-only file, the
//! `[[connector]]` entry is written (replacing one for the same relay),
//! paired mode is set when unset, and a running server is restarted in
//! place so it dials the relay now.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_protocol::wire::RemoteListenerTransport;
use toml_edit::{Item, value};

use super::{Duration, Instant};
use crate::commands::json_err::{self, CliError, codes};
use crate::commands::toml_registry;

/// The `config.toml` key the connector registry owns.
const KEY: &str = "connector";

/// How long a restarted server gets to bring the tunnel up before the
/// command reports it down (it keeps redialing either way).
const TUNNEL_WAIT: Duration = Duration::from_secs(10);
const TUNNEL_POLL: Duration = Duration::from_millis(200);

/// What `connect` did, as `--json` prints it.
#[derive(Debug, serde::Serialize)]
struct Connected {
    schema_version: u32,
    relay: String,
    token_file: PathBuf,
    cert_fingerprint: Option<String>,
    config_path: PathBuf,
    /// Whether this run set `[policy] mode = "paired"`.
    policy_set: bool,
    tunnel: Tunnel,
}

/// The tunnel as the server reported it after this run.
#[derive(Debug, serde::Serialize)]
struct Tunnel {
    /// `up`, `down` (still redialing), or `no_server`.
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub(super) fn run(
    relay: &str,
    fingerprint: Option<&str>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let socket = socket.unwrap_or_else(phux_config::socket::default_socket_path);
    match connect(relay, fingerprint, &socket) {
        Ok(done) if json => crate::output::json(&done),
        Ok(done) => {
            print_connected(&done);
            ExitCode::SUCCESS
        }
        Err(err) => json_err::emit(json, &err, 1),
    }
}

fn connect(relay: &str, fingerprint: Option<&str>, socket: &Path) -> Result<Connected, CliError> {
    let fingerprint = fingerprint
        .map(|fp| toml_registry::validate_fingerprint(fp, "relay"))
        .transpose()
        .map_err(usage)?;
    let token_file = token_path(relay);
    let entry = phux_config::ConnectorConfigEntry {
        relay: relay.to_owned(),
        token_file: Some(token_file.clone()),
        cert_fingerprint: fingerprint.clone(),
    };
    // The server's own plan check, so this accepts exactly what it dials.
    phux_server::connector::plan_connector(&entry).map_err(usage)?;
    let token = read_token()?;
    crate::commands::enroll::write_token(&token_file, &token).map_err(io)?;
    let config_path = phux_config::loader::config_path();
    upsert_connector(&config_path, &entry).map_err(registry)?;
    let policy_set = ensure_paired(&config_path)?;
    let tunnel = restart_and_wait(socket, relay);
    Ok(Connected {
        schema_version: 1,
        relay: relay.to_owned(),
        token_file,
        cert_fingerprint: fingerprint,
        config_path,
        policy_set,
        tunnel,
    })
}

/// `<state>/connectors/<relay>.token`, one per relay.
fn token_path(relay: &str) -> PathBuf {
    let mut slug = String::new();
    for c in relay.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '.' {
            c
        } else {
            '-'
        };
        if !(c == '-' && slug.ends_with('-')) {
            slug.push(c);
        }
    }
    phux_config::instance::state_dir()
        .join("connectors")
        .join(format!("{}.token", slug.trim_matches('-')))
}

/// One line of stdin, prompted on a terminal, so the secret stays out of
/// `ps` and shell history.
fn read_token() -> Result<String, CliError> {
    use std::io::{BufRead as _, IsTerminal as _, Write as _};
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprint!("Paste the tunnel token `phux relay pair` printed: ");
        let _ = std::io::stderr().flush();
    }
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .map_err(|err| io(format!("could not read the tunnel token from stdin: {err}")))?;
    let token = line.trim().to_owned();
    if !is_token(&token) {
        return Err(usage(
            "stdin did not hold a tunnel token (64 hex digits); paste the one `phux relay \
             pair --route NAME` printed on the relay"
                .to_owned(),
        ));
    }
    Ok(token)
}

fn is_token(token: &str) -> bool {
    token.len() == 2 * phux_server::auth::TOKEN_LEN && token.chars().all(|c| c.is_ascii_hexdigit())
}

/// Write `entry`, replacing a root entry for the same relay.
fn upsert_connector(
    config: &Path,
    entry: &phux_config::ConnectorConfigEntry,
) -> Result<(), String> {
    let mut doc = toml_registry::edit_document(config)?;
    let existing = toml_registry::tables_mut(&mut doc, KEY)?
        .iter()
        .position(|table| table.get("relay").and_then(Item::as_str) == Some(&entry.relay));
    if let Some(index) = existing {
        fill(toml_registry::table_mut(&mut doc, KEY, index)?, entry);
    } else {
        let mut table = toml_edit::Table::new();
        fill(&mut table, entry);
        toml_registry::tables_mut(&mut doc, KEY)?.push(table);
    }
    doc.commit()
}

/// Every field of `entry`, clearing omitted ones.
fn fill(table: &mut toml_edit::Table, entry: &phux_config::ConnectorConfigEntry) {
    table.insert("relay", value(&entry.relay));
    let path = entry
        .token_file
        .as_ref()
        .map(|path| path.display().to_string());
    toml_registry::set_or_remove(table, "token-file", path);
    toml_registry::set_or_remove(table, "cert-fingerprint", entry.cert_fingerprint.as_ref());
}

/// Set `[policy] mode = "paired"` when it is unset; `true` when written.
/// An explicit `local` is the operator's and is refused, not overridden.
fn ensure_paired(config: &Path) -> Result<bool, CliError> {
    let mode = phux_config::loader::load_from(config)
        .map(|config| config.policy.mode)
        .map_err(|err| {
            CliError::new(
                codes::INVALID_CONFIG,
                format!("{}: {err}", config.display()),
                "fix it (`phux config check`), then rerun `phux relay connect`",
            )
        })?;
    match mode {
        Some(phux_config::PolicyMode::Paired) => Ok(false),
        None => crate::commands::service::write_paired_mode(config)
            .map(|()| true)
            .map_err(registry),
        Some(phux_config::PolicyMode::Local) => Err(CliError::new(
            codes::INVALID_CONFIG,
            "[policy] mode = \"local\" admits no relay, so the server would refuse the \
             connector just written",
            format!(
                "set mode = \"paired\" in {} to let the server dial the relay",
                config.display()
            ),
        )),
    }
}

/// Restart a running server so it dials `relay`, then wait for the tunnel.
fn restart_and_wait(socket: &Path, relay: &str) -> Tunnel {
    if relay_row(socket, relay).is_err() {
        return Tunnel {
            state: "no_server",
            error: None,
        };
    }
    if let Err(err) = crate::commands::pair::rebind_server(socket) {
        return Tunnel {
            state: "down",
            error: Some(err),
        };
    }
    let deadline = Instant::now() + TUNNEL_WAIT;
    let mut last = None;
    while Instant::now() < deadline {
        match relay_row(socket, relay) {
            Ok(Some(Ok(()))) => {
                return Tunnel {
                    state: "up",
                    error: None,
                };
            }
            Ok(Some(Err(error))) => last = Some(error),
            Ok(None) | Err(()) => {}
        }
        std::thread::sleep(TUNNEL_POLL);
    }
    Tunnel {
        state: "down",
        error: last,
    }
}

/// The server's relay row for `relay`: `Err(())` when no server answers,
/// `Ok(None)` before the row exists, else up or the last dial error.
fn relay_row(socket: &Path, relay: &str) -> Result<Option<Result<(), String>>, ()> {
    let rt = crate::commands::build_cli_runtime().map_err(drop)?;
    let view = rt
        .block_on(phux_client::state::get_state(socket))
        .map_err(drop)?;
    Ok(view.snapshot().listeners().and_then(|report| {
        report
            .listeners
            .iter()
            .find(|slot| {
                slot.transport == RemoteListenerTransport::Relay
                    && slot.addr.as_deref() == Some(relay)
            })
            .map(|slot| {
                if slot.bound {
                    Ok(())
                } else {
                    Err(slot.waiting.clone().unwrap_or_default())
                }
            })
    }))
}

fn print_connected(done: &Connected) {
    outln!("This server now dials relay {}.", done.relay);
    outln!("  token   {} (owner-only)", done.token_file.display());
    outln!("  config  [[connector]] in {}", done.config_path.display());
    if done.policy_set {
        outln!("  policy  set [policy] mode = \"paired\" (a connector needs it)");
    }
    match (done.tunnel.state, &done.tunnel.error) {
        ("up", _) => outln!("  tunnel  up"),
        ("no_server", _) => {
            outln!("  tunnel  dials when the server starts (`phux server --ensure`)");
        }
        (_, Some(error)) => outln!("  tunnel  down, redialing: {error}"),
        _ => outln!("  tunnel  not up yet; `phux doctor` shows its state"),
    }
    outln!();
    outln!("Mint a device link through it with the route `phux relay pair` enrolled:");
    outln!("  phux pair --relay-route ROUTE");
}

fn usage(message: impl std::fmt::Display) -> CliError {
    CliError::new(
        codes::USAGE_ERROR,
        format!("relay connect: {message}"),
        "see `phux relay connect --help`",
    )
}

fn io(message: impl std::fmt::Display) -> CliError {
    CliError::new(
        codes::IO,
        format!("relay connect: {message}"),
        "check the state directory is writable",
    )
}

fn registry(message: impl std::fmt::Display) -> CliError {
    CliError::new(
        codes::REGISTRY,
        format!("relay connect: could not write config.toml: {message}"),
        "fix config.toml (`phux config check`), then rerun `phux relay connect`",
    )
}

#[cfg(test)]
mod tests {
    use super::{is_token, token_path};

    #[test]
    fn a_token_is_exactly_the_hex_relay_pair_prints() {
        assert!(is_token(&"ab".repeat(32)));
        assert!(!is_token(&"ab".repeat(31)));
        assert!(!is_token(&format!("{} lab", "ab".repeat(32))));
        assert!(!is_token(&"zz".repeat(32)));
    }

    #[test]
    fn each_relay_gets_its_own_token_file() {
        let name = |relay| {
            token_path(relay)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(name("relay.example:4433"), "relay.example-4433.token");
        assert_eq!(name("[::1]:4433"), "1-4433.token");
        assert_ne!(name("a.example:1"), name("a.example:2"));
    }
}
