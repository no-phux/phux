//! A distinct messaging invite wraps existing single-use certificate enrollment.

use std::fs::{File, OpenOptions};
use std::io::{BufRead as _, IsTerminal as _, Read as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use phux_client::conversation::{self, Conversation, ConversationRoute};
use phux_server::uplink::{JoinCode, Joined};
use phux_work::{Address, OperationId};
use serde::{Deserialize, Serialize};

use super::{emit, failure, messages};
use crate::commands::host::inbound::{self, MailInvitation};

const PREFIX: &str = "phux://chat/v1#";
const MAX_INVITE_BYTES: u64 = 8192;
const ENROLL_BUDGET: Duration = Duration::from_secs(8);

// Do not derive Debug: enrollment material is a single-use secret.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Invite {
    version: u8,
    id: String,
    peer_address: String,
    welcome_id: String,
    enrollment: String,
    expires_at: i64,
}

struct Created {
    record: PathBuf,
    conversation: Conversation,
    invite: String,
    ticket_id: String,
    expires_at: i64,
}

/// Remove only our still-empty reservation after a pre-enrollment failure.
/// An issued identity or completed record is never discarded by cleanup.
struct Reservation(PathBuf);

impl Drop for Reservation {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

pub(super) fn create(
    name: &str,
    hub: Option<&str>,
    welcome: &str,
    minutes: i64,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let started = Instant::now();
    match create_in(name, hub, welcome, minutes, socket) {
        Ok(created) => report_created(name, &created, started.elapsed(), json),
        Err(error) => failure(
            json,
            error,
            "use a reachable, paired phux hub; do not expose a friend's terminals with ordinary host join",
        ),
    }
}

fn create_in(
    name: &str,
    hub: Option<&str>,
    welcome: &str,
    minutes: i64,
    socket: Option<PathBuf>,
) -> Result<Created, String> {
    validate_welcome(welcome)?;
    if !(1..=60).contains(&minutes) {
        return Err("an invitation lasts 1 to 60 minutes".to_owned());
    }
    let hub = live_hub(hub, socket)?;
    let directory = conversation::directory(name)?;
    let chat = creation_identity(name, &directory, &hub)?;
    let _lock = lock_creation(&directory, &chat, &hub)?;
    let chat = publish_welcome(&directory, chat, welcome)?;
    finish_creation(&directory, chat, minutes)
}

fn lock_creation(directory: &Path, chat: &Conversation, hub: &str) -> Result<File, String> {
    let lock = File::open(directory).map_err(|err| err.to_string())?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| "another invitation creation is using this conversation".to_owned())?;
    let ConversationRoute::Hub { entry, .. } = &chat.route;
    if entry.hub != hub {
        return Err(
            "resume with the original --hub endpoint; the saved identity pins its route".to_owned(),
        );
    }
    if directory
        .join("invitation-issued")
        .try_exists()
        .map_err(|err| err.to_string())?
    {
        return Err(
            "this conversation already issued an invitation; use chat status or a new local name"
                .to_owned(),
        );
    }
    Ok(lock)
}

fn creation_identity(name: &str, directory: &Path, hub: &str) -> Result<Conversation, String> {
    let pending = directory.join("setup/conversation.json");
    if pending.try_exists().map_err(|err| err.to_string())? {
        return conversation::load_path(&pending);
    }
    let reserved = conversation::reserve(name)?;
    let _reservation = Reservation(reserved.clone());
    initialize(&reserved, hub)
}

fn publish_welcome(
    directory: &Path,
    mut chat: Conversation,
    welcome: &str,
) -> Result<Conversation, String> {
    let record = directory.join("conversation.json");
    if record.try_exists().map_err(|err| err.to_string())? {
        return conversation::load_path(&record);
    }
    let sent = messages::store_welcome(
        &chat,
        welcome.to_owned(),
        OperationId::derive_bytes(format!("chat:{}:welcome", chat.id).as_bytes()),
    )?;
    chat.welcome_id = sent.mail.id.to_hex();
    conversation::save_reserved(directory, &chat)?;
    Ok(chat)
}

/// Save the enrolled route before any mailbox exchange can fail. The staged
/// record is not a ready conversation: its welcome id is replaced on publication.
fn initialize(directory: &Path, hub: &str) -> Result<Conversation, String> {
    let id = OperationId::generate()
        .map_err(|err| err.to_string())?
        .to_hex();
    let mine = format!("chat-{}-a", &id[..16]);
    let theirs = format!("chat-{}-b", &id[..16]);
    let self_invite = mint(&mine, hub, 1)?;
    let mut enrolled = enroll_or_withdraw(&self_invite, &directory.join("identity"))?;
    enrolled.entry.mail_only = true;
    let chat = Conversation {
        version: 1,
        id: id.clone(),
        thread: format!("chat-{id}"),
        local_address: "inbox".to_owned(),
        peer_address: format!("{theirs}/inbox"),
        welcome_id: id,
        route: ConversationRoute::Hub {
            entry: enrolled.entry,
            credential_id: enrolled.credential,
        },
    };
    let pending = directory.join("setup");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&pending)
        .map_err(|err| err.to_string())?;
    conversation::save_reserved(&pending, &chat).map_err(|err| {
        format!(
            "enrolled identity remains in {}; could not save creation state: {err}",
            directory.display()
        )
    })?;
    Ok(chat)
}

fn finish_creation(directory: &Path, chat: Conversation, minutes: i64) -> Result<Created, String> {
    let ConversationRoute::Hub { entry, .. } = &chat.route;
    let peer = Address::parse(&chat.peer_address).map_err(|err| err.to_string())?;
    let theirs = peer
        .host()
        .ok_or_else(|| "missing peer namespace".to_owned())?;
    let invitation = mint(theirs, &entry.hub, minutes)?;
    let code = Invite {
        version: 1,
        id: chat.id.clone(),
        peer_address: format!("{}/{}", entry.name, chat.local_address),
        welcome_id: chat.welcome_id.clone(),
        enrollment: invitation.code.to_link(),
        expires_at: invitation.expires_at,
    };
    let result = publish_invitation(directory, &invitation, &code);
    if result.is_err() {
        inbound::withdraw_mail_invitation(&invitation.ticket_id)?;
    }
    let invite = result?;
    Ok(Created {
        record: directory.join("conversation.json"),
        conversation: chat,
        invite,
        ticket_id: invitation.ticket_id,
        expires_at: invitation.expires_at,
    })
}

fn publish_invitation(
    directory: &Path,
    invitation: &MailInvitation,
    code: &Invite,
) -> Result<String, String> {
    use std::io::Write as _;
    let encoded = code.encode()?;
    // Only the public revocation handle is kept, never the invitation secret.
    let path = directory.join("invitation-issued");
    let mut marker = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|err| err.to_string())?;
    let result = marker
        .write_all(invitation.ticket_id.as_bytes())
        .and_then(|()| marker.sync_all())
        .and_then(|()| File::open(directory)?.sync_all());
    if let Err(error) = result {
        std::fs::remove_file(&path).map_err(|err| {
            format!(
                "{error}; could not remove incomplete {}: {err}",
                path.display()
            )
        })?;
        return Err(error.to_string());
    }
    Ok(encoded)
}

fn validate_welcome(welcome: &str) -> Result<(), String> {
    if welcome.trim().is_empty() || welcome.len() > 64 * 1024 || welcome.contains('\0') {
        return Err("the welcome must be nonempty text, at most 64 KiB, with no NUL".to_owned());
    }
    Ok(())
}

fn live_hub(hub: Option<&str>, socket: Option<PathBuf>) -> Result<String, String> {
    let socket = socket.unwrap_or_else(phux_config::socket::default_socket_path);
    if crate::commands::pair::bound_quic_listener(&socket)?.is_none() {
        return Err("the selected phux server has no live QUIC listener".to_owned());
    }
    let endpoint = match hub {
        Some(hub) => hub.to_owned(),
        None => crate::commands::pair::advertised_quic_endpoint(&socket)?
            .ok_or_else(|| "pass --hub HOST:PORT that your friend can reach".to_owned())?,
    };
    phux_server::uplink::validate_hub_endpoint(&endpoint)?;
    Ok(endpoint)
}

fn mint(name: &str, hub: &str, minutes: i64) -> Result<MailInvitation, String> {
    inbound::mint_mail_invitation(name, hub, minutes).map_err(|err| err.message)
}

fn enroll_or_withdraw(invitation: &MailInvitation, dir: &Path) -> Result<Joined, String> {
    match enroll(&invitation.code, dir) {
        Ok(joined) => Ok(joined),
        Err(error) => {
            let cleanup = inbound::withdraw_mail_invitation(&invitation.ticket_id);
            let suffix = cleanup.err().map_or_else(String::new, |err| {
                format!(
                    "; could not withdraw ticket {}: {err}",
                    invitation.ticket_id
                )
            });
            Err(format!("{error}{suffix}"))
        }
    }
}

fn enroll(code: &JoinCode, dir: &Path) -> Result<Joined, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("could not start enrollment: {err}"))?;
    runtime.block_on(async {
        tokio::time::timeout(ENROLL_BUDGET, phux_server::uplink::join(code, dir))
            .await
            .map_err(|_| {
                "hub enrollment exceeded 8s; check DNS, UDP reachability and the invite expiry"
                    .to_owned()
            })?
    })
}

pub(super) fn join(name: &str, json: bool) -> ExitCode {
    let started = Instant::now();
    match join_in(name) {
        Ok((record, chat, welcome)) => {
            if json {
                emit(&serde_json::json!({
                    "state": "welcome_received",
                    "conversation": name,
                    "record": record,
                    "local_address": chat.local_address,
                    "peer_address": chat.peer_address,
                    "welcome": welcome,
                    "join_ms": started.elapsed().as_millis(),
                    "confirmation": "stored_on_hub",
                    "agent_delivery": "unverified",
                }))
            } else {
                outln!(
                    "Joined {name}. Received the welcome from {}:",
                    chat.peer_address
                );
                messages::show_message(&welcome);
                outln!(
                    "Your confirmation is stored on the hub. Agent delivery is not yet verified."
                );
                outln!("Chat: phux chat open {name}");
                outln!("Agent: phux chat run {name} -- <your agent>");
                outln!("Record: {}", record.display());
                ExitCode::SUCCESS
            }
        }
        Err(error) => failure(
            json,
            error,
            "check the hub and retry with the same conversation name; an existing saved identity is resumed, not re-enrolled",
        ),
    }
}

fn join_in(name: &str) -> Result<(PathBuf, Conversation, phux_work::Mail), String> {
    let directory = conversation::directory(name)?;
    let record = directory.join("conversation.json");
    let chat = if record.exists() {
        conversation::load_path(&record)?
    } else {
        join_new(name)?
    };
    let welcome = messages::receive_welcome(&chat)?;
    messages::confirm_welcome(&chat)?;
    Ok((record, chat, welcome))
}

fn join_new(name: &str) -> Result<Conversation, String> {
    let input = read_invite()?;
    let invitation = Invite::parse(&input)?;
    let code = JoinCode::parse(&invitation.enrollment)?;
    let directory = conversation::reserve(name)?;
    let _reservation = Reservation(directory.clone());
    let mut joined = enroll(&code, &directory.join("identity"))?;
    joined.entry.mail_only = true;
    let chat = Conversation {
        version: 1,
        thread: format!("chat-{}", invitation.id),
        id: invitation.id,
        local_address: "inbox".to_owned(),
        peer_address: invitation.peer_address,
        welcome_id: invitation.welcome_id,
        route: ConversationRoute::Hub {
            entry: joined.entry,
            credential_id: joined.credential,
        },
    };
    conversation::save_reserved(&directory, &chat).map_err(|err| {
        format!("enrolled identity is saved in {}, but its record could not be saved: {err}; keep this directory", directory.display())
    })?;
    Ok(chat)
}

fn read_invite() -> Result<String, String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        info!("Paste the single-use phux chat invite, then press Enter:");
    }
    let mut input = String::new();
    stdin
        .lock()
        .take(MAX_INVITE_BYTES + 1)
        .read_line(&mut input)
        .map_err(|_| "could not read the invitation from stdin".to_owned())?;
    if input.len() as u64 > MAX_INVITE_BYTES {
        return Err("the invitation exceeds 8 KiB".to_owned());
    }
    Ok(input.trim().to_owned())
}

impl Invite {
    fn encode(&self) -> Result<String, String> {
        serde_json::to_vec(self)
            .map(|bytes| format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
            .map_err(|_| "could not encode the chat invitation".to_owned())
    }

    fn parse(input: &str) -> Result<Self, String> {
        let encoded = input.strip_prefix(PREFIX).ok_or_else(|| {
            "not a compatible phux chat invite; use the complete phux://chat/v1# code and matching phux releases".to_owned()
        })?;
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| "the chat invitation is not valid base64url".to_owned())?;
        let invite: Self = serde_json::from_slice(&bytes)
            .map_err(|_| "the chat invitation has invalid or unsupported fields".to_owned())?;
        invite.validate()?;
        Ok(invite)
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("unsupported chat invitation version; update both phux clients".to_owned());
        }
        canonical_id(&self.id)?;
        canonical_id(&self.welcome_id)?;
        let peer = Address::parse(&self.peer_address).map_err(|err| err.to_string())?;
        if self.peer_address.trim() != self.peer_address {
            return Err("the peer address must not have surrounding whitespace".to_owned());
        }
        let code = JoinCode::parse(&self.enrollment)?;
        if peer.host().is_none() || peer.host() == Some(code.name.as_str()) {
            return Err("the invitation does not identify the other participant".to_owned());
        }
        Ok(())
    }
}

fn canonical_id(text: &str) -> Result<(), String> {
    let parsed =
        OperationId::parse(text).map_err(|_| "invalid conversation or welcome id".to_owned())?;
    if parsed.to_hex() != text {
        return Err("conversation and welcome ids must be canonical lowercase hex".to_owned());
    }
    Ok(())
}

fn report_created(name: &str, created: &Created, elapsed: Duration, json: bool) -> ExitCode {
    if json {
        return emit(&serde_json::json!({
            "state": "welcome_stored",
            "conversation": name,
            "record": created.record,
            "invite": created.invite,
            "expires_at": created.expires_at,
            "ticket_id": created.ticket_id,
            "local_address": created.conversation.local_address,
            "peer_address": created.conversation.peer_address,
            "welcome_id": created.conversation.welcome_id,
            "invite_ms": elapsed.as_millis(),
            "authority": "mail_and_claims_only",
            "peer_joined": false,
        }));
    }
    outln!("Welcome stored for {name}. Your friend has not joined yet.");
    outln!("On their machine: phux chat join <local-name>, then paste:");
    outln!("{}", created.invite);
    outln!(
        "Single use; expires at Unix time {}. Keep this invite secret.",
        created.expires_at
    );
    outln!("Check their confirmation: phux chat status {name}");
    outln!("Open the conversation: phux chat open {name}");
    outln!(
        "Withdraw unused invite: phux workload revoke {}",
        created.ticket_id
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invitation() -> Invite {
        Invite {
            version: 1,
            id: "0123456789abcdef0123456789abcdef".to_owned(),
            peer_address: "chat-0123456789abcdef-a/inbox".to_owned(),
            welcome_id: "1123456789abcdef0123456789abcdef".to_owned(),
            enrollment: JoinCode {
                hub: "hub.example:8788".to_owned(),
                server_name: None,
                ca: format!("sha256:{}", "a".repeat(64)),
                name: "chat-0123456789abcdef-b".to_owned(),
                ticket: "b".repeat(64),
            }
            .to_link(),
            expires_at: 1_800_000_000,
        }
    }

    #[test]
    fn a_messaging_invite_round_trips_but_cannot_be_used_for_terminal_join() {
        let source = invitation();
        let text = source.encode().unwrap();
        let parsed = Invite::parse(&text).unwrap();
        assert_eq!(parsed.id, source.id);
        assert_eq!(parsed.enrollment, source.enrollment);
        assert!(JoinCode::parse(&text).is_err());
    }

    #[test]
    fn bad_invites_fail_without_echoing_their_secret() {
        let mut source = invitation();
        source.version = 2;
        assert!(
            Invite::parse(&source.encode().unwrap())
                .err()
                .unwrap()
                .contains("version")
        );
        source.version = 1;
        source.peer_address = "chat-0123456789abcdef-b/inbox".to_owned();
        let error = Invite::parse(&source.encode().unwrap()).err().unwrap();
        assert!(!error.contains(&"b".repeat(64)));
        assert!(error.contains("other participant"));
        for text in ["", "phux://chat/v2#secret", "phux://chat/v1#%%%"] {
            assert!(Invite::parse(text).is_err());
        }
    }

    #[test]
    fn invalid_welcome_is_refused_before_any_invitation_is_minted() {
        for text in [
            String::new(),
            " ".to_owned(),
            "\0bad".to_owned(),
            "x".repeat(65537),
        ] {
            assert!(validate_welcome(&text).is_err());
        }
        assert!(validate_welcome("hello\nfriend").is_ok());
    }
}
