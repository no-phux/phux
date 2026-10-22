//! Join codes and `phux host join`: enrolling this server as a satellite.

use std::collections::BTreeMap;
use std::path::Path;

use phux_config::UplinkConfigEntry;
use phux_dial::CertTrust;

use super::{JOIN_DEADLINE, resolve, split_host_port};

/// The prefix of every join code.
pub const JOIN_PREFIX: &str = "phux://satellite?";

/// What `phux host ticket` prints and `phux host join` takes.
///
/// Where the hub listens, the CA it is pinned by, the satellite name, and
/// the single-use enrollment ticket. The ticket is a secret; `Debug`
/// withholds it.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinCode {
    /// The hub's QUIC listener, `HOST:PORT`.
    pub hub: String,
    /// The TLS server name to offer, when not `hub`'s host.
    pub server_name: Option<String>,
    /// The hub's workload CA, canonical `sha256:` form.
    pub ca: String,
    /// The satellite name the ticket enrolls.
    pub name: String,
    /// The ticket, 64 lowercase hex digits.
    pub ticket: String,
}

impl std::fmt::Debug for JoinCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinCode")
            .field("hub", &self.hub)
            .field("server_name", &self.server_name)
            .field("ca", &self.ca)
            .field("name", &self.name)
            .field("ticket", &"[withheld]")
            .finish()
    }
}

impl JoinCode {
    /// The code as one link: `phux://satellite?hub=…[&sni=…]&ca=…&name=…&enroll=…`.
    #[must_use]
    pub fn to_link(&self) -> String {
        let mut link = format!("{JOIN_PREFIX}hub={}", percent_encode(&self.hub));
        if let Some(sni) = &self.server_name {
            link.push_str("&sni=");
            link.push_str(&percent_encode(sni));
        }
        link.push_str("&ca=");
        link.push_str(&self.ca);
        link.push_str("&name=");
        link.push_str(&percent_encode(&self.name));
        link.push_str("&enroll=");
        link.push_str(&self.ticket);
        link
    }

    /// Parse a code [`Self::to_link`] printed. Every field but `sni` is
    /// required; unknown keys are ignored and a repeated key is refused.
    ///
    /// # Errors
    ///
    /// Wording naming the field at fault, never the ticket.
    pub fn parse(text: &str) -> Result<Self, String> {
        let trimmed = text.trim().trim_matches(|c| c == '\'' || c == '"');
        let query = trimmed
            .strip_prefix(JOIN_PREFIX)
            .ok_or_else(|| format!("a join code starts with `{JOIN_PREFIX}`"))?;
        let mut fields: BTreeMap<&str, String> = BTreeMap::new();
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            if !matches!(key, "hub" | "sni" | "ca" | "name" | "enroll") {
                continue;
            }
            if fields.insert(key, percent_decode(value)?).is_some() {
                return Err(format!("the join code repeats `{key}`"));
            }
        }
        let mut take = |key: &str| {
            fields
                .remove(key)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("the join code has no `{key}`"))
        };
        let hub = take("hub")?;
        split_host_port(&hub)?;
        let ca = take("ca")?;
        let ca = phux_config::known_authorities::canonical_authority(&ca)
            .ok_or_else(|| "the join code's `ca` is not a sha256: fingerprint".to_owned())?;
        let name = take("name")?;
        crate::workload::validate_satellite_name(&name).map_err(|err| err.to_string())?;
        let ticket = take("enroll")?;
        if ticket.len() != 64 || !ticket.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("the join code's ticket is not 64 hex digits".to_owned());
        }
        let server_name = fields.remove("sni").filter(|sni| !sni.is_empty());
        Ok(Self {
            hub,
            server_name,
            ca,
            name,
            ticket: ticket.to_ascii_lowercase(),
        })
    }
}

/// What [`join`] enrolled and stored.
#[derive(Debug, Clone)]
pub struct Joined {
    /// The `[[uplink]]` entry naming it, ready to write to `config.toml`.
    pub entry: UplinkConfigEntry,
    /// The credential id the hub recorded.
    pub credential: String,
}

/// Enroll a fresh key with `code`'s ticket and store it in `dir`.
///
/// The exchange runs over the hub's enrollment ALPN; the reply must be issued
/// by the pinned CA for this key. The pair is stored owner-only in `dir`,
/// which is created owner-only if missing.
///
/// # Errors
///
/// Wording for the operator: an unreachable hub, a refused ticket (consumed,
/// expired, revoked, or unknown), a reply that fails a check, or a storage
/// failure. Never the ticket or the key.
pub async fn join(code: &JoinCode, dir: &Path) -> Result<Joined, String> {
    let (host, port) = split_host_port(&code.hub)?;
    let addr = resolve(&host, port).await?;
    let request = crate::workload::ClientRequest::generate()
        .map_err(|err| format!("could not generate a key: {err}"))?;
    let csr = match crate::workload::ClientMaterial::from_pem(request.csr_pem().as_bytes()) {
        Ok(crate::workload::ClientMaterial::Request(der)) => der.as_ref().to_vec(),
        _ => return Err("could not encode the enrollment request".to_owned()),
    };
    let ticket = hex::decode(&code.ticket)
        .map_err(|_| "the join code's ticket is not 64 hex digits".to_owned())?;
    let dial = phux_dial::enroll::EnrollDial {
        addr,
        server_name: code.server_name.clone().unwrap_or(host),
        trust: CertTrust::Authority {
            ca: code.ca.clone(),
        },
        inner: None,
    };
    let chain = tokio::time::timeout(
        JOIN_DEADLINE,
        phux_dial::enroll::enroll(&dial, &phux_protocol::enroll::Request { ticket, csr }),
    )
    .await
    .map_err(|_| format!("the hub at {} did not answer in time", code.hub))?
    .map_err(|err| err.to_string())?;
    let issued = request.accept(&chain).map_err(|err| err.to_string())?;
    if issued.ca_fingerprint() != code.ca {
        return Err("the issuing CA is not the one the join code pins".to_owned());
    }
    ensure_private_dir(dir)?;
    let stem = format!(
        "uplink.{}.{}",
        file_safe(&code.name),
        issued
            .credential_id()
            .trim_start_matches("sha256:")
            .get(..16)
            .unwrap_or_default()
    );
    let client_cert = dir.join(format!("{stem}.pem"));
    let client_key = dir.join(format!("{stem}.key"));
    issued
        .store(&client_key, &client_cert)
        .map_err(|err| format!("could not store the satellite identity: {err}"))?;
    Ok(Joined {
        entry: UplinkConfigEntry {
            name: code.name.clone(),
            hub: code.hub.clone(),
            server_name: code.server_name.clone(),
            ca_fingerprint: code.ca.clone(),
            client_cert,
            client_key,
            enabled: true,
        },
        credential: issued.credential_id().to_owned(),
    })
}

/// Create `dir` owner-only, or check an existing one: it must be a directory
/// this user owns, and any group or other access is removed.
pub(super) fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    let existing = match std::fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(|err| format!("could not create {}: {err}", dir.display()));
        }
        Err(err) => return Err(format!("could not inspect {}: {err}", dir.display())),
    };
    if !existing.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    if existing.uid() != rustix::process::geteuid().as_raw() {
        return Err(format!(
            "{} is owned by uid {}, not this user",
            dir.display(),
            existing.uid()
        ));
    }
    if existing.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|err| format!("could not make {} owner-only: {err}", dir.display()))?;
    }
    Ok(())
}

/// A satellite name as a file-name component: anything outside
/// `[A-Za-z0-9._-]` becomes `_`.
fn file_safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':' | b'[' | b']')
        {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0F)]));
        }
    }
    out
}

fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let byte = value
                .get(index + 1..index + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(|| "the join code has a malformed %-escape".to_owned())?;
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "the join code is not UTF-8".to_owned())
}
