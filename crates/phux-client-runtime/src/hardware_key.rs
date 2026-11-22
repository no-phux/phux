//! A client key the machine's hardware holds, stored where a PEM key would
//! be (`docs/ssh.md#keys-in-hardware`).
//!
//! On a Mac the Secure Enclave generates the P-256 key and signs with it.
//! The key file holds [`HEADER`] and then the enclave's wrapped form of the
//! key in hex: opaque bytes only that Mac's enclave can use, so a copy of the
//! file signs nothing anywhere else. The key carries no user-presence
//! requirement, so an unattended reconnect signs without a prompt.
//!
//! [`identity`] is the one place a stored certificate and key become what a
//! dial presents: the two paths for a PEM key, a held identity for a hardware
//! one.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use phux_dial::TlsClientIdentity;

#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "Security.framework has no safe way to reload a key from its wrapped form"
)]
mod enclave;

/// The first line of a hardware key file. A PEM key starts `-----BEGIN`.
pub const HEADER: &str = "phux-secure-enclave-key-v1";

/// A freshly made hardware key: what to send for certification and what to
/// store.
#[derive(Debug)]
pub struct Minted {
    /// The certificate signing request the key signed, PEM.
    pub csr_pem: String,
    /// The key file's contents. Not secret: it is useless off this machine.
    pub key_file: Vec<u8>,
}

/// Whether the file at `path` is a hardware key file. Reads only as many
/// bytes as [`HEADER`] has; a missing or unreadable file is not one.
#[must_use]
pub fn is_hardware_key(path: &Path) -> bool {
    let mut prefix = [0_u8; HEADER.len()];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut prefix))
        .is_ok_and(|()| prefix == *HEADER.as_bytes())
}

/// The key file for an enclave's wrapped key.
#[cfg(target_os = "macos")]
fn encode(blob: &[u8]) -> Vec<u8> {
    use std::fmt::Write as _;
    let mut file = format!("{HEADER}\n");
    for byte in blob {
        let _ = write!(file, "{byte:02x}");
    }
    file.push('\n');
    file.into_bytes()
}

/// The wrapped key in a key file: the header line, then one line of hex.
#[cfg(any(target_os = "macos", test))]
fn decode(file: &[u8]) -> Result<Vec<u8>, String> {
    let malformed = || "the hardware key file is malformed".to_owned();
    let text = std::str::from_utf8(file).map_err(|_| malformed())?;
    let hex = text
        .strip_prefix(HEADER)
        .and_then(|rest| rest.strip_prefix('\n'))
        .map(str::trim_end)
        .filter(|hex| !hex.is_empty() && hex.len() % 2 == 0 && hex.is_ascii())
        .ok_or_else(malformed)?;
    (0..hex.len() / 2)
        .map(|index| u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|_| malformed())
}

/// Make a key in this machine's Secure Enclave and sign a certificate
/// request with it. Nothing is stored: an unused key leaves no trace.
///
/// # Errors
///
/// A machine with no Secure Enclave, or one that refused; plain wording for
/// the operator.
pub fn mint() -> Result<Minted, String> {
    #[cfg(target_os = "macos")]
    {
        use std::sync::Arc;
        let (key, blob) = enclave::EnclaveKey::create()?;
        let signer: Arc<dyn crate::enroll::DeviceSigner> = Arc::new(key);
        Ok(Minted {
            csr_pem: crate::enroll::signed_csr_pem(&signer)?,
            key_file: encode(&blob),
        })
    }
    #[cfg(not(target_os = "macos"))]
    Err("a hardware key needs a Mac with a Secure Enclave".to_owned())
}

/// The identity a dial presents for a stored `certificate` and `private_key`.
///
/// A PEM key is named by its two paths and read by the TLS stack; a hardware
/// key becomes the chain and an enclave signer. `require_paired`
/// refuses a server that does not ask for the certificate.
///
/// # Errors
///
/// A hardware key this machine's enclave cannot use (copied from another
/// Mac, altered, or read on another platform), or its unreadable chain.
pub fn identity(
    certificate: PathBuf,
    private_key: PathBuf,
    require_paired: bool,
) -> Result<TlsClientIdentity, String> {
    if is_hardware_key(&private_key) {
        let held = held(&certificate, &private_key)
            .map_err(|reason| format!("{}: {reason}", private_key.display()))?;
        return Ok(if require_paired {
            TlsClientIdentity::HeldRequirePaired(held)
        } else {
            TlsClientIdentity::Held(held)
        });
    }
    Ok(if require_paired {
        TlsClientIdentity::RequirePaired {
            certificate,
            private_key,
        }
    } else {
        TlsClientIdentity::PemFiles {
            certificate,
            private_key,
        }
    })
}

#[cfg(target_os = "macos")]
fn held(certificate: &Path, private_key: &Path) -> Result<phux_dial::HeldIdentity, String> {
    let read = |path: &Path| std::fs::read(path).map_err(|err| format!("read: {err}"));
    // ponytail: the key is not checked against the chain's leaf here; a
    // mismatched pair fails the handshake at the server. Compare the public
    // point with the leaf's if that refusal needs clearer words.
    let key = enclave::EnclaveKey::open(&decode(&read(private_key)?)?)?;
    let chain = String::from_utf8(read(certificate)?)
        .map_err(|_| "the stored certificate chain is not PEM".to_owned())?;
    crate::enroll::held_identity(&chain, std::sync::Arc::new(key))
}

#[cfg(not(target_os = "macos"))]
fn held(_certificate: &Path, _private_key: &Path) -> Result<phux_dial::HeldIdentity, String> {
    Err("this is a Secure Enclave key, which only the Mac that made it can use".to_owned())
}

#[cfg(test)]
mod tests;
