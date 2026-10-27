//! `phux host minimal` — Minimal (<https://minimal.dev>) boxes as satellites
//! of this hub (ADR-0182's inbound join, delivered for the box).
//!
//! A box is a Linux microVM or namespace whose loopback is its own, so the
//! hub cannot dial it; the box can reach this machine's loopback as
//! `host.min.internal`. `setup` writes a Minimal loadout that patches a Linux
//! `phux` into every box and starts its server on activation, makes that
//! loadout a default, and installs a per-user service running `watch`.
//! `sync` (and `watch`, its loop) mints a single-use ticket for each running
//! box that has not joined and hands the join code to `phux host join -`
//! inside the box over `min session exec`'s stdin, so the code is never in a
//! file or an argv. A destroyed box's satellite entry and credential go with
//! it. Minimal has no host-side hook, which is why the hub watches.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use usage::{Args, Subcommands};

use super::super::json_err::{self, CliError, codes};
use super::super::satellite as satellite_registry;
use super::hub::reload_local_hub;
use super::inbound;

mod boxes;
mod files;
mod hub;
mod service;
#[cfg(test)]
mod tests;

use boxes::{BoxSession, MinCli};
use files::{BoxRegistry, MinimalDirs};

/// The loadout `setup` writes and adds to `default_loadouts`.
const LOADOUT: &str = "phux";

/// Every satellite this integration registers is named `min-…`.
const SATELLITE_PREFIX: &str = "min-";

/// Minutes a join code minted for a box stays redeemable; it is spent within
/// seconds, so this only bounds a failed delivery.
const TICKET_MINUTES: i64 = 5;

/// How long `watch` waits before trying a box whose join failed again.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// How long `watch` waits before probing a box that had no phux again: one
/// probed while still activating may not have its loadout's files yet.
const REPROBE_AFTER: Duration = Duration::from_secs(5);

/// `phux host minimal <action>`.
#[derive(Debug, Subcommands)]
pub(crate) enum MinimalAction {
    /// Make every Minimal box join this hub.
    ///
    /// Writes the `phux` Minimal loadout, which copies a Linux phux into
    /// each box and starts its server, adds it to Minimal's
    /// `default_loadouts`, makes this machine's service a federation hub,
    /// and installs a per-user service running `phux host minimal watch`.
    /// On macOS the box binary is this release channel's published Linux
    /// phux, downloaded and signature-checked like `phux update`. Run it
    /// again to refresh the box binary or after the hub's QUIC port changes.
    /// The hub must run `[policy] mode = "paired"` with a QUIC listener on
    /// loopback or all addresses.
    Setup {
        #[usage(flatten)]
        opts: MinimalOpts,

        /// The Linux phux to copy into boxes, instead of the published one
        /// (macOS) or this binary (Linux).
        #[usage(long, value_name = "PATH")]
        box_phux: Option<PathBuf>,

        /// Skip the watcher service; run `phux host minimal watch` yourself.
        #[usage(long)]
        no_service: bool,
    },

    /// Join each running box that has not joined, and forget destroyed ones.
    ///
    /// A box joins as satellite `min-NAME`, NAME being its Minimal session
    /// name. A box without phux on its `PATH` (activated without the `phux`
    /// loadout) is skipped.
    Sync {
        #[usage(flatten)]
        opts: MinimalOpts,
    },

    /// Run `sync` every few seconds until stopped; what the service runs.
    Watch {
        #[usage(flatten)]
        opts: MinimalOpts,

        /// Seconds between passes.
        #[usage(
            long,
            value_name = "SECS",
            default = "2",
            default_value_t = 2,
            validate = "int(value) >= 1 && int(value) <= 3600",
            validate_error = "must be between 1 and 3600 seconds"
        )]
        interval: u64,
    },

    /// Undo `setup`: stop the watcher, remove the loadout, forget every box.
    ///
    /// Running boxes keep their phux; they stop reaching this hub because
    /// their credentials are revoked.
    Remove,
}

/// Flags every `phux host minimal` action takes.
#[derive(Debug, Args)]
pub(crate) struct MinimalOpts {
    /// `HOST:PORT` boxes dial. Omitted: `host.min.internal` and the port of
    /// this server's live QUIC listener.
    #[usage(long, value_name = "HOST:PORT")]
    hub: Option<String>,

    /// The `min` CLI. Omitted: `min` on `PATH`, else `~/.local/bin/min`.
    #[usage(long, value_name = "PATH")]
    min: Option<PathBuf>,
}

pub(super) fn run(action: &MinimalAction) -> ExitCode {
    let result = match action {
        MinimalAction::Setup {
            opts,
            box_phux,
            no_service,
        } => setup(opts, box_phux.as_deref(), *no_service),
        MinimalAction::Sync { opts } => {
            Context::new(opts).and_then(|ctx| ctx.sync(&mut Memory::default()))
        }
        MinimalAction::Watch { opts, interval } => {
            Context::new(opts).and_then(|ctx| ctx.watch(Duration::from_secs(*interval)))
        }
        MinimalAction::Remove => remove(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => json_err::emit(false, &err, 1),
    }
}

/// What `sync` needs: the `min` CLI, the address boxes dial, and the record
/// of which box joined as which satellite.
struct Context {
    min: MinCli,
    hub: String,
    registry: BoxRegistry,
}

impl Context {
    fn new(opts: &MinimalOpts) -> Result<Self, CliError> {
        Ok(Self {
            min: MinCli::resolve(opts.min.as_deref()).map_err(no_min)?,
            hub: hub::endpoint(opts.hub.as_deref())?,
            registry: BoxRegistry::at(files::registry_path()),
        })
    }

    /// One pass. `memory` carries what earlier `watch` passes learned, so a
    /// box is probed and reported once and a failing one is retried slowly.
    fn sync(&self, memory: &mut Memory) -> Result<(), CliError> {
        let sessions = self.min.sessions().map_err(no_min)?;
        let mut joined = self.registry.load().map_err(state_failure)?;
        let live: BTreeSet<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        memory.forget_gone(&live);
        self.prune(&mut joined, &live)?;
        for session in sessions.iter().filter(|s| s.is_active()) {
            if !joined.contains_key(&session.id) && !memory.waits_on(&session.id) {
                self.admit(session, &mut joined, memory)?;
            }
        }
        Ok(())
    }

    /// Forget every joined box that is no longer `live`.
    fn prune(
        &self,
        joined: &mut BTreeMap<String, String>,
        live: &BTreeSet<&str>,
    ) -> Result<(), CliError> {
        let gone: Vec<String> = joined
            .keys()
            .filter(|id| !live.contains(id.as_str()))
            .cloned()
            .collect();
        for id in gone {
            let Some(satellite) = joined.remove(&id) else {
                continue;
            };
            forget_box(&satellite);
            self.registry.save(joined).map_err(state_failure)?;
            outln!("{satellite}: box destroyed; removed it and revoked its credential");
        }
        Ok(())
    }

    /// Join one box that has not joined, or note why it cannot.
    fn admit(
        &self,
        session: &BoxSession,
        joined: &mut BTreeMap<String, String>,
        memory: &mut Memory,
    ) -> Result<(), CliError> {
        if !self.min.has_phux(&session.id) {
            if memory.lacks_phux(&session.id) {
                outln!(
                    "{}: box has no phux; activate it with the `{LOADOUT}` loadout to join it",
                    session.label()
                );
            }
            return Ok(());
        }
        memory.no_phux.remove(&session.id);
        let taken: BTreeSet<&str> = joined.values().map(String::as_str).collect();
        let satellite = satellite_name(session, &taken);
        if let Err(reason) = self.join(session, &satellite) {
            if memory.fail(&session.id, &reason) {
                warn!("{satellite}: could not join (retrying every 30s): {reason}");
            }
            return Ok(());
        }
        memory.failed.remove(&session.id);
        joined.insert(session.id.clone(), satellite.clone());
        self.registry.save(joined).map_err(state_failure)?;
        outln!("{satellite}: joined (panes appear as {satellite}/@N)");
        Ok(())
    }

    /// Mint `satellite`'s code and redeem it inside `session`'s box; a failed
    /// delivery withdraws the ticket and the entry it registered.
    fn join(&self, session: &BoxSession, satellite: &str) -> Result<(), String> {
        let (code, ticket) =
            inbound::mint_code(satellite, &self.hub, TICKET_MINUTES).map_err(|err| err.message)?;
        self.min.join(&session.id, &code).inspect_err(|_| {
            if let Err(err) = inbound::withdraw_ticket(&ticket) {
                warn!("{satellite}: could not withdraw ticket {ticket} ({err})");
            }
            forget_box(satellite);
        })
    }

    fn watch(&self, interval: Duration) -> Result<(), CliError> {
        info!(
            "watching Minimal boxes every {}s; boxes dial {}",
            interval.as_secs(),
            self.hub
        );
        let mut memory = Memory::default();
        let mut last_error = None;
        loop {
            // A pass that fails (Minimal's daemon restarting, a config being
            // rewritten) is reported once and retried, never fatal.
            match self.sync(&mut memory) {
                Ok(()) => last_error = None,
                Err(err) if last_error.as_ref() != Some(&err.message) => {
                    warn!("{}", err.message);
                    last_error = Some(err.message);
                }
                Err(_) => {}
            }
            std::thread::sleep(interval);
        }
    }
}

/// What `watch` remembers between passes.
#[derive(Debug, Default)]
struct Memory {
    /// Boxes that had no phux, and when they were last probed.
    no_phux: BTreeMap<String, Instant>,
    /// Boxes whose last join failed: when, and why.
    failed: BTreeMap<String, (Instant, String)>,
}

impl Memory {
    /// Whether this pass leaves box `id` alone.
    fn waits_on(&self, id: &str) -> bool {
        self.no_phux
            .get(id)
            .is_some_and(|at| at.elapsed() < REPROBE_AFTER)
            || self
                .failed
                .get(id)
                .is_some_and(|(at, _)| at.elapsed() < RETRY_AFTER)
    }

    /// Record a probe that found no phux; `true` the first time, the one
    /// worth reporting.
    fn lacks_phux(&mut self, id: &str) -> bool {
        self.no_phux.insert(id.to_owned(), Instant::now()).is_none()
    }

    /// Record a failed join; `true` when `reason` is new and worth reporting.
    fn fail(&mut self, id: &str, reason: &str) -> bool {
        let previous = self
            .failed
            .insert(id.to_owned(), (Instant::now(), reason.to_owned()));
        previous.is_none_or(|(_, old)| old != reason)
    }

    fn forget_gone(&mut self, live: &BTreeSet<&str>) {
        self.no_phux.retain(|id, _| live.contains(id.as_str()));
        self.failed.retain(|id, _| live.contains(id.as_str()));
    }
}

/// `min-NAME` for a box, NAME being its session name (else its id's first
/// eight characters) with each character a satellite name refuses replaced.
/// A name `taken` by another box gets the id prefix appended.
fn satellite_name(session: &BoxSession, taken: &BTreeSet<&str>) -> String {
    let short_id: String = session.id.chars().take(8).collect();
    let base = session
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&short_id);
    let clean: String = base
        .chars()
        .map(|c| match c {
            '/' | ':' => '-',
            c if c.is_control() || c.is_whitespace() => '-',
            c => c,
        })
        .take(200)
        .collect();
    let name = format!("{SATELLITE_PREFIX}{clean}");
    if taken.contains(name.as_str()) {
        format!("{name}-{short_id}")
    } else {
        name
    }
}

/// Remove `satellite`'s `inbound://` entry and revoke its credentials. An
/// entry this integration did not register (any other endpoint) is left.
fn forget_box(satellite: &str) {
    let entries = match satellite_registry::load_registry() {
        Ok(entries) => entries,
        Err(err) => {
            warn!("{satellite}: could not read the satellite registry ({err})");
            return;
        }
    };
    if let Some(entry) = entries.iter().find(|entry| {
        entry.name == satellite && phux_server::hub::is_inbound_endpoint(&entry.endpoint)
    }) && let Err(err) = satellite_registry::remove_entry(entry)
    {
        warn!("{satellite}: could not remove its satellite entry ({err})");
    }
    inbound::revoke_satellite(satellite);
    let _ = reload_local_hub();
}

fn setup(opts: &MinimalOpts, box_phux: Option<&Path>, no_service: bool) -> Result<(), CliError> {
    let ctx = Context::new(opts)?;
    install_loadout(&box_binary(box_phux)?)?;
    report_local_hub(&super::super::service::ensure_local_hub());
    let unit = if no_service {
        None
    } else {
        Some(service::install(&ctx.min, opts.hub.as_deref()).map_err(setup_failure)?)
    };
    match &unit {
        Some(unit) => outln!("Watcher service installed: {}", shown(unit)),
        None => outln!("Skipped the watcher service; run `phux host minimal watch` to join boxes."),
    }
    outln!();
    outln!(
        "Boxes dial {}. Every `min session activate` now joins this hub:",
        ctx.hub
    );
    outln!("  phux ls          lists each box's sessions under min-NAME");
    outln!("  phux attach min-NAME/@1, or the desktop app, opens its panes");
    // The service's first pass joins the boxes already running; a second
    // pass here would race it for the same names.
    if unit.is_some() {
        return Ok(());
    }
    outln!();
    ctx.sync(&mut Memory::default())
}

/// Write the loadout patching `binary` into boxes and make it a default.
fn install_loadout(binary: &Path) -> Result<(), CliError> {
    let dirs = MinimalDirs::from_env().map_err(setup_failure)?;
    dirs.write_loadout(LOADOUT, binary).map_err(setup_failure)?;
    outln!(
        "Wrote the `{LOADOUT}` Minimal loadout: {}",
        shown(&dirs.loadout_file(LOADOUT))
    );
    outln!("  boxes get {}", shown(binary));
    if dirs.add_default_loadout(LOADOUT).map_err(setup_failure)? {
        outln!(
            "Added `{LOADOUT}` to default_loadouts in {}",
            shown(&dirs.config_file())
        );
    }
    Ok(())
}

fn report_local_hub(hub: &super::super::service::LocalHub) {
    use super::super::service::LocalHub;
    match hub {
        LocalHub::Already => outln!("This machine's phux service already runs as a hub."),
        LocalHub::Patched => outln!(
            "Made this machine's phux service a hub; it takes effect when that server next starts."
        ),
        LocalHub::Installed => outln!(
            "Wrote a hub service for this machine; phux starts it once no server is running."
        ),
        LocalHub::Skipped(reason) => warn!(
            "could not make this machine's phux service a hub ({reason}); run the server with --hub"
        ),
    }
}

fn remove() -> Result<(), CliError> {
    match service::uninstall() {
        Ok(Some(unit)) => outln!("Removed the watcher service {}", shown(&unit)),
        Ok(None) => {}
        Err(err) => warn!("could not remove the watcher service: {err}"),
    }
    let dirs = MinimalDirs::from_env().map_err(setup_failure)?;
    if dirs.remove_loadout(LOADOUT).map_err(setup_failure)? {
        outln!("Removed the `{LOADOUT}` Minimal loadout and its default_loadouts entry");
    }
    let registry = BoxRegistry::at(files::registry_path());
    let joined = registry.load().map_err(state_failure)?;
    for satellite in joined.values() {
        forget_box(satellite);
        outln!("{satellite}: removed and revoked");
    }
    registry.save(&BTreeMap::new()).map_err(state_failure)
}

/// The Linux phux boxes get: `--box-phux`, this binary on Linux, else the
/// release channel's published build for this machine's architecture.
fn box_binary(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        return std::path::absolute(path)
            .ok()
            .filter(|path| path.is_file())
            .ok_or_else(|| setup_failure(format!("--box-phux {} is not a file", path.display())));
    }
    if cfg!(target_os = "linux") {
        return std::env::current_exe()
            .map_err(|err| setup_failure(format!("could not resolve this binary: {err}")));
    }
    let target = format!("{}-unknown-linux-gnu", std::env::consts::ARCH);
    info!("downloading the published phux for {target} boxes");
    files::fetch_box_binary(&target).map_err(setup_failure)
}

/// `path` with the home directory spelled `~`, for messages.
fn shown(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    home.as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
        .map_or_else(
            || path.display().to_string(),
            |rest| format!("~/{}", rest.display()),
        )
}

fn no_min(detail: String) -> CliError {
    CliError::new(
        codes::NO_SERVER,
        detail,
        "install Minimal (curl -fsSL https://go.minimal.dev/stable | sh), or pass --min PATH",
    )
}

fn setup_failure(detail: String) -> CliError {
    CliError::new(
        codes::INTERNAL_ERROR,
        detail,
        "check the path's permissions, then rerun `phux host minimal setup`",
    )
}

fn state_failure(detail: impl std::fmt::Display) -> CliError {
    CliError::new(
        codes::INTERNAL_ERROR,
        format!("could not use the joined-box record: {detail}"),
        "check the state directory's permissions, then rerun",
    )
}
