//! The client half of the enrollment ALPN.
//!
//! See `workload-auth.md` §8.2 and ADR-0154: one QUIC connection under
//! [`ENROLL_ALPN`] carrying one request, and the checks every client applies
//! to the reply before it stores anything.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use phux_protocol::enroll::{MAX_REPLY, Reply, Request};
use phux_protocol::policy::ENROLL_ALPN;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, UnixTime};

use crate::DialError;
use crate::tls::{CertTrust, TlsClientIdentity};

/// Where an enrollment goes: the server's QUIC listener and how to trust it.
#[derive(Debug, Clone)]
pub struct EnrollDial {
    /// The listener's `HOST:PORT`.
    pub addr: SocketAddr,
    /// TLS server name offered in SNI.
    pub server_name: String,
    /// How to trust the server's certificate (its CA pin when the link
    /// carried one, ADR-0153); through a relay, how to trust the relay.
    pub trust: CertTrust,
    /// Through a relay: the server, end to end (ADR-0154 item 5). The
    /// exchange then runs inside that session, under the enrollment ALPN.
    pub inner: Option<crate::quic::InnerTls>,
}

/// Send one enrollment request and return the issued PEM chain, unchecked;
/// pass it to [`check_issued_chain`] before storing it.
///
/// # Errors
///
/// [`DialError::AuthRefused`] when the server refused (a consumed, expired,
/// or unknown ticket, or a rejected request), and the usual dial errors.
pub async fn enroll(d: &EnrollDial, request: &Request) -> Result<String, DialError> {
    let encoded = request
        .encode()
        .map_err(|err| DialError::Connect(format!("enrollment request: {err}")))?;
    if let Some(inner) = &d.inner {
        return enroll_relayed(d, inner, &encoded).await;
    }
    let bind = if d.addr.is_ipv6() {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
    };
    let mut endpoint = quinn::Endpoint::client(bind)
        .map_err(|err| DialError::Connect(format!("bind QUIC client socket: {err}")))?;
    let (config, refusal) =
        crate::quic::client_config(&d.trust, Some(&TlsClientIdentity::None), ENROLL_ALPN)?;
    endpoint.set_default_client_config(config);
    let conn = endpoint
        .connect(d.addr, &d.server_name)
        .map_err(|err| DialError::Connect(format!("dial {}: {err}", d.addr)))?
        .await
        .map_err(|err| {
            crate::tls::authority_refusal(&refusal)
                .unwrap_or_else(|| crate::quic::close_error(&err))
        })?;
    let exchanged = async {
        let (mut send, mut recv) = conn.open_bi().await.map_err(io_lost)?;
        send.write_all(&encoded).await.map_err(io_lost)?;
        send.finish().map_err(io_lost)?;
        recv.read_to_end(MAX_REPLY).await.map_err(io_lost)
    }
    .await;
    let reply = match exchanged {
        Ok(bytes) => bytes,
        Err(err) => {
            return Err(conn
                .close_reason()
                .map_or(err, |close| crate::quic::close_error(&close)));
        }
    };
    conn.close(0_u32.into(), b"done");
    endpoint.wait_idle().await;
    decoded(&reply)
}

/// The exchange inside the end-to-end session through a relay: the request
/// is self-delimiting, and the server ends its side after the reply.
async fn enroll_relayed(
    d: &EnrollDial,
    inner: &crate::quic::InnerTls,
    encoded: &[u8],
) -> Result<String, DialError> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let outer = crate::QuicDial {
        addr: d.addr,
        server_name: d.server_name.clone(),
        token: None,
        trust: d.trust.clone(),
        identity: Some(TlsClientIdentity::None),
        inner: None,
    };
    let (endpoint, conn, mut stream) =
        crate::quic::dial_relayed(&outer, inner, ENROLL_ALPN).await?;
    // Both messages are self-delimiting: a relay ends the whole splice when
    // either side finishes, so nobody half-closes before the reply is read.
    let exchanged = async {
        stream.write_all(encoded).await?;
        stream.flush().await?;
        let mut head = [0_u8; 5];
        stream.read_exact(&mut head).await?;
        let len = usize::try_from(u32::from_be_bytes([head[1], head[2], head[3], head[4]]))
            .unwrap_or(usize::MAX);
        if len > phux_protocol::enroll::MAX_REPLY_BODY {
            return Err(std::io::Error::other("oversized enrollment reply"));
        }
        let mut reply = head.to_vec();
        reply.resize(5 + len, 0);
        stream.read_exact(&mut reply[5..]).await?;
        Ok::<_, std::io::Error>(reply)
    }
    .await;
    conn.close(0_u32.into(), b"done");
    endpoint.wait_idle().await;
    let reply = exchanged.map_err(io_lost)?;
    decoded(&reply)
}

fn decoded(reply: &[u8]) -> Result<String, DialError> {
    match Reply::decode(reply)
        .map_err(|err| DialError::Connect(format!("enrollment reply: {err}")))?
    {
        Reply::Issued(chain) => Ok(chain),
        Reply::Refused => Err(DialError::AuthRefused(
            "enrollment refused: the ticket is consumed, expired, or unknown, or the request was rejected"
                .to_owned(),
        )),
    }
}

fn io_lost(err: impl std::fmt::Display) -> DialError {
    DialError::Connect(format!("enrollment exchange: {err}"))
}

/// Check an issued chain as `phux host add` does, and return it parsed.
///
/// `workload-auth.md` §8.1 step 3: exactly two certificates, the leaf
/// carrying `public_key` (the requester's DER `SubjectPublicKeyInfo`) and
/// verifying as a client certificate under the second, valid now, and that
/// second the pinned authority when `pinned_authority` names one.
///
/// # Errors
///
/// A static reason; the reply itself is never echoed.
pub fn check_issued_chain(
    chain_pem: &str,
    public_key: &[u8],
    pinned_authority: Option<&str>,
) -> Result<Vec<CertificateDer<'static>>, &'static str> {
    if chain_pem.contains("PRIVATE KEY") {
        return Err("the reply carries a private key");
    }
    let chain = CertificateDer::pem_slice_iter(chain_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "the reply is not a PEM certificate chain")?;
    let [leaf, ca] = chain.as_slice() else {
        return Err("the reply is not exactly a leaf and its CA");
    };
    let parsed = rustls::server::ParsedCertificate::try_from(leaf)
        .map_err(|_| "the issued certificate does not parse")?;
    if parsed.subject_public_key_info().as_ref() != public_key {
        return Err("the issued certificate does not carry this device's key");
    }
    if let Some(pinned) = pinned_authority
        && !same_authority(&crate::tls::authority_fingerprint(ca), pinned)
    {
        return Err("the issuing CA is not the one this server's certificate chains to");
    }
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(ca.clone())
        .map_err(|_| "the issuing CA does not parse")?;
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        std::sync::Arc::new(roots),
        std::sync::Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()
    .map_err(|_| "the issuing CA cannot verify client certificates")?;
    verifier
        .verify_client_cert(leaf, &[], UnixTime::now())
        .map_err(|_| "the issued certificate is not a client certificate valid now")?;
    Ok(chain)
}

fn same_authority(a: &str, b: &str) -> bool {
    let digits = |pin: &str| -> String {
        let pin = pin.trim();
        pin.strip_prefix("sha256:")
            .unwrap_or(pin)
            .chars()
            .filter(char::is_ascii_hexdigit)
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (a, b) = (digits(a), digits(b));
    a.len() == 64 && a == b
}
