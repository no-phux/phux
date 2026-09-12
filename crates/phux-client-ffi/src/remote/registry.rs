//! Caller-bounded snapshots of the CLI's machine registry. No network or token I/O.
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::{mem, ptr};

use phux_config::{RemoteConfigEntry, SatelliteConfigEntry};

use super::{
    PhuxRemoteTunnel, REMOTE_TUNNEL_RESOLVED, Shared, config_path_in, guard, target, text_in,
};
use crate::error::{BridgeError, check_struct};
use crate::types::{PhuxBytes, PhuxClientResult, bytes_out};

/// Bounds chosen by the embedder. Zero limits are invalid; exhaustion is explicit.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxMachineRegistryOptions {
    pub size: usize,
    pub version: u32,
    pub config_path: PhuxBytes,
    pub max_entries: usize,
    pub max_file_bytes: usize,
}

/// Borrowed snapshot information. `message` never includes parser source text.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxMachineRegistryInfo {
    pub size: usize,
    pub version: u32,
    pub count: usize,
    /// Zero for a usable snapshot, one for a failed load.
    pub failed: u32,
    pub message: PhuxBytes,
}

/// Borrowed row; strings remain valid until the registry is freed.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxMachineRecord {
    pub size: usize,
    pub version: u32,
    /// 1 remote, 2 satellite. This Mac is a native projection, not a registration.
    pub role: u32,
    /// 1 direct, 2 needs setup, 3 via hub, 4 disabled, 5 unsupported.
    pub route: u32,
    pub name: PhuxBytes,
    pub endpoint: PhuxBytes,
    pub session: PhuxBytes,
    pub message: PhuxBytes,
}

struct Row {
    role: u32,
    route: u32,
    name: String,
    endpoint: String,
    session: String,
    message: String,
}

struct Contents {
    raw: String,
    remote: Vec<RemoteConfigEntry>,
    satellites: Vec<SatelliteConfigEntry>,
}

/// Immutable capture, owned by one embedder thread. Free cancels its authority.
pub struct PhuxMachineRegistry {
    path: PathBuf,
    max_entries: usize,
    max_file_bytes: usize,
    contents: Option<Contents>,
    rows: Vec<Row>,
    message: String,
}

impl std::fmt::Debug for PhuxMachineRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PhuxMachineRegistry")
            .field("count", &self.rows.len())
            .field("failed", &self.contents.is_none())
            .finish_non_exhaustive()
    }
}

fn read_contents(path: &Path, max_entries: usize, max_bytes: usize) -> Result<Contents, String> {
    let mut raw = String::new();
    match std::fs::File::open(path) {
        Ok(file) => {
            file.take(max_bytes as u64 + 1)
                .read_to_string(&mut raw)
                .map_err(|_| "could not read machine registry")?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("could not open machine registry".to_owned()),
    }
    if raw.len() > max_bytes {
        return Err("machine registry exceeds caller byte budget".to_owned());
    }
    let cfg = phux_config::parse_with_defaults(&raw, path)
        .map_err(|_| "machine registry is malformed or an inherited configuration is unavailable; edit Phux configuration")?;
    if cfg.remote.len().saturating_add(cfg.satellites.len()) > max_entries {
        return Err("machine registry exceeds caller entry budget".to_owned());
    }
    Ok(Contents {
        raw,
        remote: cfg.remote,
        satellites: cfg.satellites,
    })
}

fn direct_route(entry: &RemoteConfigEntry) -> (u32, String) {
    if entry.endpoint.starts_with("ssh://") {
        return (2, "SSH route requires setup in a local terminal".to_owned());
    }
    let target = target::RemoteTarget {
        user: None,
        host: entry.name.clone(),
        port: None,
    };
    if target::classify(&entry.endpoint, &target).is_err() {
        return (
            5,
            "unsupported endpoint; edit this saved machine".to_owned(),
        );
    }
    (1, String::new())
}

// URLs with embedded credentials are not a display surface. Pins and token-file
// paths remain internal, and even malformed input cannot echo source text.
fn display_endpoint(endpoint: &str) -> Option<&str> {
    if endpoint.contains(['?', '#']) {
        return None;
    }
    let authority = endpoint.split_once("://")?.1.split('/').next()?;
    if authority
        .split_once('@')
        .is_some_and(|(user, _)| user.contains(':'))
    {
        return None;
    }
    Some(endpoint)
}

fn protect_row(mut row: Row) -> Row {
    if display_endpoint(&row.endpoint).is_none() {
        "(endpoint requires configuration repair)".clone_into(&mut row.endpoint);
        row.route = 5;
        "Credential-bearing or malformed endpoint is not displayed; edit Phux configuration"
            .clone_into(&mut row.message);
    }
    row
}

fn rows(contents: &Contents) -> Vec<Row> {
    let remotes = contents.remote.iter().map(|entry| {
        let (route, message) = direct_route(entry);
        protect_row(Row {
            role: 1,
            route,
            name: entry.name.clone(),
            endpoint: entry.endpoint.clone(),
            session: entry.session.clone().unwrap_or_default(),
            message,
        })
    });
    let satellites = contents.satellites.iter().map(|entry| {
        protect_row(Row {
            role: 2,
            route: if entry.enabled { 3 } else { 4 },
            name: entry.name.clone(),
            endpoint: entry.endpoint.clone(),
            session: String::new(),
            message: if entry.enabled {
                "Reached through this Mac's Phux hub; browse its sessions"
            } else {
                "Satellite is disabled in Phux configuration"
            }
            .to_owned(),
        })
    });
    let mut rows: Vec<_> = remotes.chain(satellites).collect();
    let mut seen = std::collections::BTreeSet::new();
    let duplicates: std::collections::BTreeSet<_> = rows
        .iter()
        .filter(|row| !seen.insert((row.role, row.name.clone())))
        .map(|row| (row.role, row.name.clone()))
        .collect();
    for row in &mut rows {
        if duplicates.contains(&(row.role, row.name.clone())) {
            row.route = 5;
            "Duplicate saved name in this role; repair Phux configuration"
                .clone_into(&mut row.message);
        }
    }
    rows
}

impl PhuxMachineRegistry {
    fn open(path: PathBuf, max_entries: usize, max_file_bytes: usize) -> Self {
        let mut registry = Self {
            path,
            max_entries,
            max_file_bytes,
            contents: None,
            rows: Vec::new(),
            message: String::new(),
        };
        match read_contents(&registry.path, max_entries, max_file_bytes) {
            Ok(contents) => {
                registry.rows = rows(&contents);
                registry.contents = Some(contents);
            }
            Err(message) => registry.message = message,
        }
        registry
    }

    fn validate(&self, index: usize) -> Result<&Row, BridgeError> {
        let saved = self
            .contents
            .as_ref()
            .ok_or_else(|| BridgeError::state("registry unavailable"))?;
        let current = read_contents(&self.path, self.max_entries, self.max_file_bytes)
            .map_err(BridgeError::state)?;
        if current.raw != saved.raw
            || current.remote != saved.remote
            || current.satellites != saved.satellites
        {
            return Err(BridgeError::state(
                "machine registry changed; refresh Machines",
            ));
        }
        self.rows
            .get(index)
            .ok_or_else(|| BridgeError::invalid("row is out of bounds"))
    }

    fn resolve(&self, index: usize) -> Result<PhuxRemoteTunnel, BridgeError> {
        let row = self.validate(index)?;
        if row.role != 1 || row.route != 1 {
            return Err(BridgeError::state(
                "machine does not support a direct connection",
            ));
        }
        let entry = &self
            .contents
            .as_ref()
            .ok_or_else(|| BridgeError::state("registry unavailable"))?
            .remote[index];
        let target = target::RemoteTarget {
            user: None,
            host: entry.name.clone(),
            port: None,
        };
        let (endpoint, transport) =
            target::classify(&entry.endpoint, &target).map_err(BridgeError::state)?;
        let resolved = target::Resolved {
            name: entry.name.clone(),
            endpoint: endpoint.clone(),
            session: entry.session.clone().filter(|s| !s.trim().is_empty()),
            transport,
            token_file: entry.token_file.clone(),
            cert_fingerprint: entry.cert_fingerprint.clone(),
        };
        Ok(PhuxRemoteTunnel {
            name: entry.name.clone(),
            endpoint,
            session: row.session.clone(),
            resolved: Some(resolved),
            shared: std::sync::Arc::new(Shared::with_state(REMOTE_TUNNEL_RESOLVED)),
            cancel: std::sync::Arc::new(tokio::sync::Notify::new()),
            thread: std::sync::Mutex::new(None),
        })
    }

    fn forget(&self, index: usize) -> Result<(), BridgeError> {
        let row = self.validate(index)?;
        if row.route == 5 {
            return Err(BridgeError::state(
                "repair ambiguous or unsupported entry before forgetting",
            ));
        }
        let contents = self
            .contents
            .as_ref()
            .ok_or_else(|| BridgeError::state("registry unavailable"))?;
        phux_config::toml_registry::forget_machine(
            &self.path,
            &contents.raw,
            if row.role == 1 {
                "remote"
            } else {
                "satellites"
            },
            &row.name,
            &row.endpoint,
        )
        .map_err(BridgeError::state)
    }
}

/// Open a bounded immutable snapshot. A load failure returns OK with info.failed=1.
/// # Safety
/// Options/spans must be readable and out writable for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_machine_registry_open(
    options: *const PhuxMachineRegistryOptions,
    out: *mut *mut PhuxMachineRegistry,
) -> PhuxClientResult {
    guard(|| {
        // SAFETY: caller contract; null checked before accessing.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("out is null"))?;
        *out = ptr::null_mut();
        // SAFETY: caller contract; null checked before accessing.
        let options =
            unsafe { options.as_ref() }.ok_or_else(|| BridgeError::invalid("options is null"))?;
        check_struct(
            options.size,
            mem::size_of::<PhuxMachineRegistryOptions>(),
            options.version,
        )?;
        // SAFETY: options' span follows the caller contract.
        let path = unsafe { options_path(options) }?;
        *out = Box::into_raw(Box::new(PhuxMachineRegistry::open(
            path,
            options.max_entries,
            options.max_file_bytes,
        )));
        Ok(())
    })
}

// SAFETY: config_path must be readable for its declared length.
unsafe fn options_path(options: &PhuxMachineRegistryOptions) -> Result<PathBuf, BridgeError> {
    if options.max_entries == 0
        || options.max_file_bytes == 0
        || options.max_file_bytes > isize::MAX as usize
    {
        return Err(BridgeError::invalid("invalid registry budgets"));
    }
    // SAFETY: forwarded span contract; text_in rejects null and oversized spans.
    let path = unsafe {
        text_in(
            options.config_path,
            super::MAX_CONFIG_PATH_BYTES,
            "config path",
        )
    }?;
    Ok(config_path_in(path)?.map_or_else(phux_config::loader::config_path, Path::to_path_buf))
}

/// Read snapshot metadata; borrowed spans last until free.
/// # Safety
/// Registry must be live and out writable with initialized size/version.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_machine_registry_info(
    registry: *const PhuxMachineRegistry,
    out: *mut PhuxMachineRegistryInfo,
) -> PhuxClientResult {
    guard(|| {
        // SAFETY: caller contract; nulls checked.
        let registry =
            unsafe { registry.as_ref() }.ok_or_else(|| BridgeError::invalid("registry is null"))?;
        // SAFETY: caller contract; null checked.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("out is null"))?;
        check_struct(
            out.size,
            mem::size_of::<PhuxMachineRegistryInfo>(),
            out.version,
        )?;
        out.count = registry.rows.len();
        out.failed = u32::from(registry.contents.is_none());
        out.message = bytes_out(registry.message.as_bytes());
        Ok(())
    })
}

/// Read one caller-selected row. Out-of-range is `INVALID_ARGUMENT`.
/// # Safety
/// Registry must be live and out writable with initialized size/version.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_machine_registry_get(
    registry: *const PhuxMachineRegistry,
    index: usize,
    out: *mut PhuxMachineRecord,
) -> PhuxClientResult {
    guard(|| {
        // SAFETY: caller contract; null checked.
        let registry =
            unsafe { registry.as_ref() }.ok_or_else(|| BridgeError::invalid("registry is null"))?;
        // SAFETY: caller contract; null checked.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("out is null"))?;
        check_struct(out.size, mem::size_of::<PhuxMachineRecord>(), out.version)?;
        let row = registry
            .rows
            .get(index)
            .ok_or_else(|| BridgeError::invalid("row is out of bounds"))?;
        out.role = row.role;
        out.route = row.route;
        out.name = bytes_out(row.name.as_bytes());
        out.endpoint = bytes_out(row.endpoint.as_bytes());
        out.session = bytes_out(row.session.as_bytes());
        out.message = bytes_out(row.message.as_bytes());
        Ok(())
    })
}

/// Re-read the registry and refuse stale captured rows, including inherited edits.
/// # Safety
/// Registry must be live for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_machine_registry_validate(
    registry: *const PhuxMachineRegistry,
    index: usize,
) -> PhuxClientResult {
    guard(|| {
        // SAFETY: caller contract; null checked.
        let registry =
            unsafe { registry.as_ref() }.ok_or_else(|| BridgeError::invalid("registry is null"))?;
        registry.validate(index).map(|_| ())
    })
}

/// Resolve the exact captured remote after freshness validation. No network I/O.
///
/// The resulting tunnel holds the checked endpoint, pin and credential path;
/// starting it never resolves its name again. Caller owns it via `tunnel_free`.
/// # Safety
/// Registry must be live; out must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_machine_registry_resolve(
    registry: *const PhuxMachineRegistry,
    index: usize,
    out: *mut *mut PhuxRemoteTunnel,
) -> PhuxClientResult {
    guard(|| {
        // SAFETY: caller contract; null checked.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("out is null"))?;
        *out = ptr::null_mut();
        // SAFETY: caller contract; null checked.
        let registry =
            unsafe { registry.as_ref() }.ok_or_else(|| BridgeError::invalid("registry is null"))?;
        *out = Box::into_raw(Box::new(registry.resolve(index)?));
        Ok(())
    })
}

/// Forget the exact saved entry. Caller must first disconnect its connection.
/// Successful mutation invalidates this snapshot; open a new one.
/// # Safety
/// Registry must be live and exclusively owned during the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_machine_registry_forget(
    registry: *const PhuxMachineRegistry,
    index: usize,
) -> PhuxClientResult {
    guard(|| {
        // SAFETY: caller contract; null checked.
        let registry =
            unsafe { registry.as_ref() }.ok_or_else(|| BridgeError::invalid("registry is null"))?;
        registry.forget(index)
    })
}

/// Free a snapshot; null is a no-op.
/// # Safety
/// Must be null or a live uniquely owned snapshot, freed once without concurrent calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_machine_registry_free(registry: *mut PhuxMachineRegistry) {
    if !registry.is_null() {
        // SAFETY: caller transfers unique ownership exactly once.
        drop(unsafe { Box::from_raw(registry) });
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
