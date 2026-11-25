//! What a human `phux pair` prints: the link and how to use it first, then
//! one block of facts (expiry, credential, ticket, pins), then the QR and
//! where the token went. `--json` never comes here.

use std::path::Path;
use std::process::ExitCode;

use phux_server::workload::tickets::MintedTicket;

use super::{
    LINK_SECONDS, LinkOutput, SecretSurface, format_rfc3339, hides_bearer, legacy_connect_link,
    print_camera_qr, secret_surface,
};

/// The first phux iOS and Android release that reads secrets from the fragment.
const MIN_FRAGMENT_APP: &str = "0.4.83";

/// The width of a fact's label column.
const LABEL_WIDTH: usize = 11;

/// One labeled line of the facts block, never a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Fact {
    pub(super) label: &'static str,
    pub(super) value: String,
}

impl Fact {
    pub(super) fn new(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
        }
    }
}

/// The facts a direct (listener) mint reports about the server it reaches.
pub(super) fn listener_pins(
    fingerprint: Option<&str>,
    authority: Option<&str>,
    overlay: &[std::net::IpAddr],
) -> Vec<Fact> {
    let mut pins = Vec::new();
    if let Some(fingerprint) = fingerprint {
        pins.push(Fact::new("Server pin", format!("SHA-256 {fingerprint}")));
    }
    if let Some(authority) = authority {
        pins.push(Fact::new(
            "Server CA",
            format!("{authority} (pinned; survives leaf renewal)"),
        ));
    }
    if !overlay.is_empty() {
        let addresses: Vec<String> = overlay.iter().map(ToString::to_string).collect();
        pins.push(Fact::new("Overlay", addresses.join(", ")));
    }
    pins
}

/// The facts a relay mint reports: the relay the device dials and pins, and
/// the server it verifies end to end through it.
pub(super) fn relay_pins(
    relay: &str,
    route: &str,
    fingerprint: Option<&str>,
    authority: &str,
) -> Vec<Fact> {
    let relay_pin = fingerprint.map_or_else(
        || "none configured: a loopback relay only".to_owned(),
        |fingerprint| format!("SHA-256 {fingerprint} (the relay sees only ciphertext)"),
    );
    vec![
        Fact::new("Relay", format!("{relay}, route \"{route}\"")),
        Fact::new("Relay pin", relay_pin),
        Fact::new(
            "Server CA",
            format!("{authority} (verified end to end through the relay)"),
        ),
    ]
}

/// The fields a human mint may show. `token` reaches stdout only when the
/// mint has no link to carry it, and neither `token` nor `link` is printed
/// when `hide_bearer` is set.
pub(super) struct HumanSecret<'a> {
    pub(super) credential_id: &'a str,
    pub(super) token: &'a str,
    pub(super) fingerprint: Option<&'a str>,
    pub(super) link: Option<&'a str>,
    /// The name the link lists the server under, for the paste-ready command.
    pub(super) host_name: &'a str,
    pub(super) ticket: Option<&'a MintedTicket>,
    /// What the device pins and dials, printed among the facts.
    pub(super) pins: Vec<Fact>,
    pub(super) expires_at: Option<jiff::Timestamp>,
    pub(super) expires_label: Option<&'a str>,
    pub(super) first_use_by: Option<jiff::Timestamp>,
    pub(super) qr: bool,
    /// Render the QR beneath the text link (never with `hide_bearer`).
    pub(super) default_qr: bool,
    pub(super) secret_file: Option<&'a Path>,
    pub(super) surface: SecretSurface,
    pub(super) hide_bearer: bool,
}

impl<'a> HumanSecret<'a> {
    /// A mint with nothing shown yet beyond what `output` decides; the caller
    /// fills in the link, pins, and ticket it has.
    pub(super) const fn new(
        credential_id: &'a str,
        token: &'a str,
        output: LinkOutput<'a>,
    ) -> Self {
        Self {
            credential_id,
            token,
            fingerprint: None,
            link: None,
            host_name: "",
            ticket: None,
            pins: Vec::new(),
            expires_at: output.expires_at,
            expires_label: output.expires_label,
            first_use_by: output.first_use_by,
            qr: output.qr,
            default_qr: output.default_qr,
            secret_file: output.secret_file,
            surface: secret_surface(output),
            hide_bearer: hides_bearer(output.qr, output.secret_file.is_some(), output.json),
        }
    }
}

/// Present the mint, then say where the token went (scripts read that line).
pub(super) fn finish_human_mint(secret: &HumanSecret<'_>, tokens: &Path) -> ExitCode {
    let shown = if secret.hide_bearer {
        present_hidden(secret)
    } else {
        present_text(secret);
        true
    };
    if !shown {
        return ExitCode::FAILURE;
    }
    outln!("Token written to {}", tokens.display());
    ExitCode::SUCCESS
}

/// The terminal mint: the link card (or, with no link, the token), the
/// facts, the notes, then the QR.
fn present_text(secret: &HumanSecret<'_>) {
    match secret.link {
        Some(link) => print_link_card(secret, link),
        None => print_token_block(secret.token),
    }
    print_facts(secret);
    print_notes(secret);
    // A QR that cannot render has already warned; the text link stands.
    if let Some(link) = secret.link.filter(|_| secret.default_qr) {
        print_camera_qr(link);
    }
}

/// The link, marked secret once, and how each kind of device uses it.
fn print_link_card(secret: &HumanSecret<'_>, link: &str) {
    outln!("Connect link (a secret: it carries the pairing token):");
    outln!("  {link}");
    outln!();
    let scan = if secret.default_qr {
        ", or scan the QR below"
    } else {
        ""
    };
    print_fact(
        "Phone",
        &format!("open the link on the phone{scan} (phux app {MIN_FRAGMENT_APP} or newer)"),
    );
    print_fact("Computer", &computer_command(secret.host_name, link));
    outln!();
}

/// What another computer pastes: register the server from the link, then
/// attach to it. `host add` refuses a spent or expired link and registers
/// nothing, where `attach --remote --code` would register a host that cannot
/// enroll.
fn computer_command(host_name: &str, link: &str) -> String {
    let name = command_name(host_name);
    format!("phux host add {name} --code '{link}' && phux attach {name}")
}

/// `host_name` when it is a plain word `phux host add` takes, else `NAME`.
fn command_name(host_name: &str) -> &str {
    let plain = host_name.starts_with(|first: char| first.is_ascii_alphanumeric())
        && host_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if plain && super::super::remote_target::parse_target(host_name).is_ok() {
        host_name
    } else {
        "NAME"
    }
}

/// With no link to carry it, the bearer itself is what the operator hands over.
fn print_token_block(token: &str) {
    outln!("Pairing token (a secret — give it to the device once):");
    outln!("  {token}");
    outln!();
}

fn print_facts(secret: &HumanSecret<'_>) {
    print_fact("Expires", &expiry_text(secret));
    print_fact(
        "Revoke",
        &format!(
            "phux pair revoke {} (a paired device works until then)",
            secret.credential_id
        ),
    );
    if let Some(ticket) = secret.ticket {
        print_fact("Ticket", &ticket_text(secret, ticket));
    }
    for pin in &secret.pins {
        print_fact(pin.label, &pin.value);
    }
    outln!();
}

fn print_fact(label: &str, value: &str) {
    outln!("  {label:<LABEL_WIDTH$}{value}");
}

/// When the credential stops working, in words (#2398).
pub(super) fn expiry_text(secret: &HumanSecret<'_>) -> String {
    if let Some(deadline) = secret.first_use_by {
        return format!(
            "{} ({} minutes) unless a device pairs first; SINGLE USE",
            format_rfc3339(deadline),
            LINK_SECONDS / 60
        );
    }
    match (secret.expires_at.map(format_rfc3339), secret.expires_label) {
        (Some(stamp), Some(label)) => format!("{stamp} ({label} from mint)"),
        (Some(stamp), None) => stamp,
        (None, _) => "NEVER: any device holding it can connect until it is revoked".to_owned(),
    }
}

/// The enrollment ticket's handle, where its secret is, and, when the
/// credential outlives it, when it lapses.
fn ticket_text(secret: &HumanSecret<'_>, ticket: &MintedTicket) -> String {
    let held = match (secret.surface, secret.link.is_some()) {
        (SecretSurface::Text, true) => "in the link",
        (SecretSurface::Text, false) => "not printed: no connect link carries it",
        (SecretSurface::Qr, _) => "in the QR",
        (SecretSurface::File, _) => "in the secret file",
        (SecretSurface::QrAndFile, _) => "in the QR and the secret file",
    };
    let text = format!(
        "{} {held}; the device enrolls its own key with it",
        ticket.id
    );
    if secret.first_use_by.is_some() {
        return text;
    }
    let when = jiff::Timestamp::from_second(ticket.expires_at)
        .map_or_else(|_| format!("unix {}", ticket.expires_at), format_rfc3339);
    format!("{text} until {when}")
}

/// The lines below the facts: how to widen a single-use mint, and the
/// spelling an app build without Universal Links opens.
fn print_notes(secret: &HumanSecret<'_>) {
    let mut notes = Vec::new();
    if secret.first_use_by.is_some() {
        notes.push("`--reusable` or `--expires DURATION` mints a link any device can use.");
    }
    if secret.link.and_then(legacy_connect_link).is_some() {
        notes.push(
            "An app build without Universal Links opens it with phux://connect in place of https://phux.sh/connect.",
        );
    }
    for note in &notes {
        outln!("{note}");
    }
    if !notes.is_empty() {
        outln!();
    }
}

/// The camera and secret-file mint: a banner, the facts, then the secret
/// file and the QR. The bearer never reaches the text.
fn present_hidden(secret: &HumanSecret<'_>) -> bool {
    print_hidden_banner(secret);
    print_facts(secret);
    let filed = secret
        .secret_file
        .is_none_or(|path| file_the_secret(path, secret));
    let scanned = !secret.qr || show_camera_qr(secret.link);
    filed && scanned
}

fn print_hidden_banner(secret: &HumanSecret<'_>) {
    outln!("================================================================");
    outln!("PAIRING — the bearer is not printed on this screen.");
    if secret.expires_at.is_none() && secret.first_use_by.is_none() {
        outln!("Do not film a credential that does not expire.");
    }
    outln!("================================================================");
    outln!();
}

fn file_the_secret(path: &Path, secret: &HumanSecret<'_>) -> bool {
    match write_secret_file(path, secret) {
        Ok(()) => {
            outln!(
                "Secret written to {} (mode 0600). The bearer is not shown here.",
                path.display()
            );
            outln!();
            true
        }
        Err(err) => {
            error!(
                "pair: credential {} was minted but the secret file could not be written: {err}",
                secret.credential_id
            );
            hint!(
                "pair: the bearer was not printed. Delete {} if it exists, revoke with `phux pair revoke {}`, and retry.",
                path.display(),
                secret.credential_id
            );
            false
        }
    }
}

fn show_camera_qr(link: Option<&str>) -> bool {
    let Some(link) = link else {
        warn!("pair: no connect link to render; nothing scannable was printed");
        return false;
    };
    if !print_camera_qr(link) {
        return false;
    }
    // The link keeps its secrets after `#` (#2396); app builds older than
    // this read only the query and refuse it as malformed.
    outln!(
        "Scan with phux app {MIN_FRAGMENT_APP} or newer; an older one says \"not a phux link\" until it is updated."
    );
    outln!();
    true
}

pub(super) fn secret_file_body(secret: &HumanSecret<'_>) -> String {
    use std::fmt::Write;
    let mut body = String::new();
    let expires = secret
        .expires_at
        .map_or_else(|| "never".to_owned(), format_rfc3339);
    let _ = write!(
        body,
        "# phux pairing secret. Mode 0600. Delete after the device has paired.\n\
         credential_id={}\n\
         expires_at={expires}\n\
         token={}\n",
        secret.credential_id, secret.token
    );
    if let Some(deadline) = secret.first_use_by {
        let _ = writeln!(body, "link_expires_at={}", format_rfc3339(deadline));
    }
    if let Some(link) = secret.link {
        let _ = writeln!(body, "connect_link={link}");
        if let Some(legacy) = legacy_connect_link(link) {
            let _ = writeln!(body, "legacy_connect_link={legacy}");
        }
    }
    if let Some(fingerprint) = secret.fingerprint {
        let _ = writeln!(body, "cert_fingerprint={fingerprint}");
    }
    body
}

pub(super) fn write_secret_file(path: &Path, secret: &HumanSecret<'_>) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let body = secret_file_body(secret);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|err| format!("could not create {}: {err}", path.display()))?;
    file.write_all(body.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{command_name, computer_command};

    /// The paste-ready command names the server when its name is a plain
    /// word, and quotes the link as one shell word.
    #[test]
    fn the_computer_command_is_paste_ready() {
        let link = "https://phux.sh/connect?quic=quic://r:1&sni=lab#token=ab";
        assert_eq!(
            computer_command("lab", link),
            format!("phux host add lab --code '{link}' && phux attach lab")
        );
        assert_eq!(command_name("mini-2.local"), "mini-2.local");
        for odd in ["studio mini", "", "-x", "a/b", "me@box", "it's"] {
            assert_eq!(command_name(odd), "NAME", "{odd:?}");
        }
    }
}
