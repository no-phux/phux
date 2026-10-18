//! `phux mail` and `phux claim`: agent coordination in the work store
//! (ADR-0188).
//!
//! Both talk to the work store's keeper, which `phux mission` also uses and
//! starts on demand. `mail send` then rings each recipient's pane: one
//! acknowledged, single-line prompt naming the sender and thread, so an idle
//! agent wakes and reads. Mail is stored before the bell rings, and a bell
//! that cannot ring (no server, no agent in the pane) never loses the mail.
//!
//! Who you are: `--as`, else `$PHUX_AGENT_ADDRESS`, else this pane (`@N`
//! from `$PHUX_TERMINAL_ID`), else `$USER`.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::agent_meta::AgentRecord;
use phux_client::agent_prompt::prompt_agent;
use phux_protocol::ResourceId;
use phux_protocol::ids::InputOperationId;
use phux_server::runtime::default_socket_path;
use phux_work::{
    AckMail, Address, Claim, ClaimFilter, ClaimOutcome, Mail, ReadMail, ReleaseClaim, RenewClaim,
    SendMail, TakeClaim,
};

use super::json_err::{self, codes};

/// Schema of every `phux mail` and `phux claim` JSON document.
const SCHEMA: &str = "phux.coord/v1";
/// Default claim lifetime.
const DEFAULT_TTL_SECS: u64 = 60 * 60;
/// Longest preview of a message in a doorbell prompt.
const PREVIEW_CHARS: usize = 100;
/// Overrides who this process speaks as.
const ADDRESS_ENV: &str = "PHUX_AGENT_ADDRESS";

#[derive(Debug, usage::Subcommands)]
pub(crate) enum MailAction {
    /// Send a message and ring each recipient's pane
    Send {
        /// Recipients: a pane (`@749`), a labelled pane (`pi@749`), or a name.
        #[usage(required, value_name = "TO")]
        to: Vec<String>,
        /// The message. `-` reads it from stdin.
        #[usage(short = 'm', long, value_name = "TEXT")]
        message: String,
        /// Thread slug, such as the branch or task the message is about.
        #[usage(short = 't', long, value_name = "SLUG")]
        thread: String,
        /// Store the message without prompting any pane.
        #[usage(long)]
        no_ring: bool,
        #[usage(flatten)]
        me: AsOpt,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Read your unread mail and mark it read
    Read {
        /// Only this thread, including what you sent in it.
        #[usage(short = 't', long, value_name = "SLUG")]
        thread: Option<String>,
        /// Include messages already read.
        #[usage(long)]
        all: bool,
        /// Do not mark what is shown as read.
        #[usage(long)]
        peek: bool,
        #[usage(flatten)]
        me: AsOpt,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Acknowledge a message, so its sender knows it was handled
    Ack {
        /// Message id, as `read` prints it.
        #[usage(value_name = "ID")]
        id: String,
        #[usage(flatten)]
        me: AsOpt,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
}

#[derive(Debug, usage::Subcommands)]
pub(crate) enum ClaimAction {
    /// Claim paths, all or none; exits 1 naming the holders on a conflict
    Take {
        /// Paths to claim. Each covers everything under it.
        #[usage(required, value_name = "PATH")]
        paths: Vec<String>,
        /// Share with shared acquisitions; taking never renews a live lease.
        #[usage(long)]
        shared: bool,
        /// Seconds until the claim lapses unless renewed. Default 3600, at most 86400.
        #[usage(long, value_name = "SECS")]
        ttl: Option<u64>,
        /// Why, shown to anyone the claim blocks.
        #[usage(long, value_name = "TEXT")]
        note: Option<String>,
        #[usage(flatten)]
        me: AsOpt,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Renew a live acquisition without changing its paths or sharing mode
    Renew {
        /// The fence returned when this acquisition was granted.
        #[usage(value_name = "FENCE")]
        fence: u64,
        /// New lifetime from now. Default 3600, at most 86400 seconds.
        #[usage(long, value_name = "SECS")]
        ttl: Option<u64>,
        #[usage(flatten)]
        me: AsOpt,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Release precisely one acquisition; repeating stale cleanup is harmless
    Release {
        /// The fence returned when this acquisition was granted.
        #[usage(value_name = "FENCE")]
        fence: u64,
        #[usage(flatten)]
        me: AsOpt,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// List live claims
    List {
        /// Only claims covering this path or under it.
        #[usage(long, value_name = "PATH")]
        path: Option<String>,
        /// Only this holder's.
        #[usage(long, value_name = "ADDRESS")]
        holder: Option<String>,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
}

#[derive(Debug, usage::Args)]
pub(crate) struct AsOpt {
    /// Speak as this address instead of this pane.
    #[usage(long = "as", value_name = "ADDRESS")]
    me: Option<String>,
}

/// Who this process speaks as.
fn whoami(me: &AsOpt) -> String {
    let env = |name| {
        std::env::var(name)
            .ok()
            .filter(|value: &String| !value.is_empty())
    };
    me.me
        .clone()
        .or_else(|| env(ADDRESS_ENV))
        .or_else(|| env("PHUX_TERMINAL_ID").map(|id| terminal_address(&id)))
        .unwrap_or_else(super::mission::actor)
}

fn terminal_address(value: &str) -> String {
    format!("@{}", value.strip_prefix('@').unwrap_or(value))
}

pub(crate) fn run_mail(action: &MailAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        MailAction::Send {
            to,
            message,
            thread,
            no_ring,
            me,
            json,
        } => send(
            to,
            message,
            thread,
            *no_ring,
            &whoami(me),
            json.json,
            socket,
        ),
        MailAction::Read {
            thread,
            all,
            peek,
            me,
            json,
        } => read(thread.clone(), *all, *peek, &whoami(me), json.json),
        MailAction::Ack { id, me, json } => ack(id, &whoami(me), json.json),
    }
}

pub(crate) fn run_claim(action: &ClaimAction) -> ExitCode {
    match action {
        ClaimAction::Take {
            paths,
            shared,
            ttl,
            note,
            me,
            json,
        } => take(
            paths,
            *shared,
            *ttl,
            note.as_deref(),
            &whoami(me),
            json.json,
        ),
        ClaimAction::Renew {
            fence,
            ttl,
            me,
            json,
        } => renew(*fence, *ttl, &whoami(me), json.json),
        ClaimAction::Release { fence, me, json } => release(*fence, &whoami(me), json.json),
        ClaimAction::List { path, holder, json } => {
            list(path.as_deref(), holder.clone(), json.json)
        }
    }
}

fn store(json: bool) -> Result<phux_work::Client, ExitCode> {
    super::mission::client(json)
}

fn emit(document: &serde_json::Value) {
    match serde_json::to_string(document) {
        Ok(line) => outln!("{line}"),
        Err(err) => error!("could not encode the result: {err}"),
    }
}

fn usage_error(json: bool, message: impl Into<String>, remedy: &str) -> ExitCode {
    json_err::emit(
        json,
        &json_err::CliError::new(codes::USAGE_ERROR, message, remedy),
        crate::exit_codes::EXIT_USAGE,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "one argument per `mail send` input"
)]
fn send(
    to: &[String],
    message: &str,
    thread: &str,
    no_ring: bool,
    me: &str,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let body = if message == "-" {
        let mut text = String::new();
        if let Err(err) = std::io::stdin().read_to_string(&mut text) {
            return usage_error(json, format!("could not read stdin: {err}"), "pass -m TEXT");
        }
        text
    } else {
        message.to_owned()
    };
    let client = match store(json) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let operation_id = match phux_work::OperationId::generate() {
        Ok(id) => id,
        Err(err) => return super::mission::report_as(json, &err, SCHEMA),
    };
    let sent = match client.send_mail(SendMail {
        thread: thread.to_owned(),
        from: me.to_owned(),
        to: to.to_vec(),
        body,
        operation_id,
    }) {
        Ok(sent) => sent,
        Err(err) => return super::mission::report_as(json, &err, SCHEMA),
    };
    let bells: Vec<(String, String)> = if no_ring {
        Vec::new()
    } else {
        ring(&sent.mail, me, socket)
    };
    if json {
        emit(&serde_json::json!({
            "schema": SCHEMA,
            "mail": sent.mail,
            "bells": bells.iter().map(|(to, outcome)| serde_json::json!({"to": to, "outcome": outcome})).collect::<Vec<_>>(),
        }));
    } else {
        outln!(
            "sent {} to {}",
            sent.mail.id.to_hex(),
            sent.mail.to.join(", ")
        );
        for (to, outcome) in &bells {
            outln!("  {to}: {outcome}");
        }
    }
    ExitCode::SUCCESS
}

/// The one line a doorbell types into a recipient's agent.
fn doorbell(mail: &Mail) -> String {
    let first = mail
        .body
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let mut preview: String = first
        .chars()
        .filter(|ch| !ch.is_control())
        .take(PREVIEW_CHARS)
        .collect();
    if first.chars().count() > PREVIEW_CHARS || mail.body.lines().nth(1).is_some() {
        preview.push('…');
    }
    format!(
        "[phux mail] {} in thread {}: {preview} (read: phux mail read --thread {}; reply: phux mail send {} -t {} -m ...)",
        mail.from, mail.thread, mail.thread, mail.from, mail.thread
    )
}

/// Ring every recipient pane other than the sender's. Each outcome is one
/// word or a short reason; none of them fails the send.
fn ring(mail: &Mail, me: &str, socket: Option<PathBuf>) -> Vec<(String, String)> {
    let me = Address::parse(me).ok();
    let panes: Vec<(String, u32)> = mail
        .to
        .iter()
        .filter_map(|to| {
            let address = Address::parse(to).ok()?;
            if me.as_ref().is_some_and(|me| me.key() == address.key()) {
                return None;
            }
            let pane = address.key().strip_prefix('@')?.parse().ok()?;
            Some((to.clone(), pane))
        })
        .collect();
    if panes.is_empty() {
        return Vec::new();
    }
    let Ok(rt) = super::cli_runtime() else {
        return panes
            .into_iter()
            .map(|(to, _)| (to, "not rung: no async runtime".to_owned()))
            .collect();
    };
    let socket = socket.unwrap_or_else(default_socket_path);
    let text = doorbell(mail);
    rt.block_on(async {
        let mut outcomes = Vec::with_capacity(panes.len());
        for (to, pane) in panes {
            let Some(operation_id) = InputOperationId::new(uuid::Uuid::new_v4().into_bytes())
            else {
                outcomes.push((to, "not rung: no operation id".to_owned()));
                continue;
            };
            let accept = |_: &AgentRecord| None;
            let outcome = prompt_agent(
                &socket,
                &ResourceId::local(pane),
                &text,
                operation_id,
                &accept,
                None,
            )
            .await;
            outcomes.push((
                to,
                match outcome {
                    Ok(_) => "rung".to_owned(),
                    Err(err) => format!("not rung: {err}"),
                },
            ));
        }
        outcomes
    })
}

fn read(thread: Option<String>, all: bool, peek: bool, me: &str, json: bool) -> ExitCode {
    let client = match store(json) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let mail = match client.read_mail(ReadMail {
        reader: me.to_owned(),
        thread,
        all,
        mark_read: !peek,
        limit: 0,
    }) {
        Ok(mail) => mail,
        Err(err) => return super::mission::report_as(json, &err, SCHEMA),
    };
    if json {
        emit(&serde_json::json!({"schema": SCHEMA, "reader": me, "mail": mail}));
    } else if mail.is_empty() {
        outln!("no mail for {me}");
    } else {
        for message in &mail {
            outln!(
                "{}  {}  {} -> {}  [{}]{}",
                message.created_at,
                message.id.to_hex(),
                message.from,
                message.to.join(", "),
                message.thread,
                if message.acked_at.is_some() {
                    "  acked"
                } else {
                    ""
                }
            );
            for line in message.body.lines() {
                outln!("    {line}");
            }
        }
    }
    ExitCode::SUCCESS
}

fn ack(id: &str, me: &str, json: bool) -> ExitCode {
    let Ok(id) = serde_json::from_value(serde_json::Value::from(id)) else {
        return usage_error(
            json,
            format!("not a message id: {id}"),
            "use the 32 hex digits `phux mail read` prints",
        );
    };
    let client = match store(json) {
        Ok(client) => client,
        Err(code) => return code,
    };
    match client.ack_mail(AckMail {
        reader: me.to_owned(),
        id,
    }) {
        Ok(mail) => {
            if json {
                emit(&serde_json::json!({"schema": SCHEMA, "mail": mail}));
            } else {
                outln!("acknowledged {} from {}", mail.id.to_hex(), mail.from);
            }
            ExitCode::SUCCESS
        }
        Err(err) => super::mission::report_as(json, &err, SCHEMA),
    }
}

/// A claimed path as the store wants it: absolute and normalized, resolved
/// against the current directory, following no symlinks.
fn absolute(path: &str) -> Result<String, String> {
    let path = std::path::Path::new(path);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| format!("could not read the current directory: {err}"))?
            .join(path)
    };
    let mut normal = PathBuf::from("/");
    for component in joined.components() {
        match component {
            std::path::Component::ParentDir => {
                normal.pop();
            }
            std::path::Component::Normal(part) => normal.push(part),
            std::path::Component::RootDir
            | std::path::Component::CurDir
            | std::path::Component::Prefix(_) => {}
        }
    }
    normal
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{} is not UTF-8", joined.display()))
}

fn absolutes(json: bool, paths: &[String]) -> Result<Vec<String>, ExitCode> {
    paths
        .iter()
        .map(|path| absolute(path))
        .collect::<Result<_, _>>()
        .map_err(|message| usage_error(json, message, "pass an absolute path"))
}

fn print_claims(claims: &[Claim]) {
    let now = u64::try_from(chrono::Utc::now().timestamp()).unwrap_or(0);
    for claim in claims {
        outln!(
            "{}  {}  {} fence {}  {}m left{}",
            claim.path,
            claim.holder,
            if claim.exclusive {
                "exclusive"
            } else {
                "shared"
            },
            claim.fence,
            claim.expires_at.saturating_sub(now).div_ceil(60),
            if claim.note.is_empty() {
                String::new()
            } else {
                format!("  {}", claim.note)
            }
        );
    }
}

fn take(
    paths: &[String],
    shared: bool,
    ttl: Option<u64>,
    note: Option<&str>,
    me: &str,
    json: bool,
) -> ExitCode {
    let paths = match absolutes(json, paths) {
        Ok(paths) => paths,
        Err(code) => return code,
    };
    let client = match store(json) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let outcome = match client.take_claim(TakeClaim {
        holder: me.to_owned(),
        paths,
        exclusive: !shared,
        ttl_secs: ttl.unwrap_or(DEFAULT_TTL_SECS),
        note: note.unwrap_or_default().to_owned(),
    }) {
        Ok(outcome) => outcome,
        Err(err) => return super::mission::report_as(json, &err, SCHEMA),
    };
    if json {
        emit(&serde_json::json!({"schema": SCHEMA, "holder": me, "claim": outcome}));
    }
    match outcome {
        ClaimOutcome::Granted(claims) => {
            if !json {
                print_claims(&claims);
            }
            ExitCode::SUCCESS
        }
        ClaimOutcome::Conflict(claims) => {
            if !json {
                error!("a live acquisition overlaps; nothing was claimed");
                print_claims(&claims);
                hint!(
                    "ask the holder with `phux mail send HOLDER -t THREAD -m ...`, or wait for it to lapse"
                );
            }
            ExitCode::from(1)
        }
    }
}

fn renew(fence: u64, ttl: Option<u64>, me: &str, json: bool) -> ExitCode {
    let client = match store(json) {
        Ok(client) => client,
        Err(code) => return code,
    };
    match client.renew_claim(RenewClaim {
        holder: me.to_owned(),
        fence,
        ttl_secs: ttl.unwrap_or(DEFAULT_TTL_SECS),
    }) {
        Ok(claims) => {
            if json {
                emit(&serde_json::json!({"schema": SCHEMA, "holder": me, "claims": claims}));
            } else {
                print_claims(&claims);
            }
            ExitCode::SUCCESS
        }
        Err(err) => super::mission::report_as(json, &err, SCHEMA),
    }
}
fn release(fence: u64, me: &str, json: bool) -> ExitCode {
    let client = match store(json) {
        Ok(client) => client,
        Err(code) => return code,
    };
    match client.release_claim(ReleaseClaim {
        holder: me.to_owned(),
        fence,
    }) {
        Ok(released) => {
            if json {
                emit(&serde_json::json!({"schema": SCHEMA, "holder": me, "released": released}));
            } else {
                outln!("released {released}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => super::mission::report_as(json, &err, SCHEMA),
    }
}

fn list(path: Option<&str>, holder: Option<String>, json: bool) -> ExitCode {
    let path = match path.map(absolute).transpose() {
        Ok(path) => path,
        Err(message) => return usage_error(json, message, "pass an absolute path"),
    };
    let client = match store(json) {
        Ok(client) => client,
        Err(code) => return code,
    };
    match client.claims(ClaimFilter { path, holder }) {
        Ok(claims) => {
            if json {
                emit(&serde_json::json!({"schema": SCHEMA, "claims": claims}));
            } else if claims.is_empty() {
                outln!("no live claims");
            } else {
                print_claims(&claims);
            }
            ExitCode::SUCCESS
        }
        Err(err) => super::mission::report_as(json, &err, SCHEMA),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mail(body: &str) -> Mail {
        serde_json::from_value(serde_json::json!({
            "id": "0123456789abcdef0123456789abcdef",
            "thread": "desktop-ux",
            "from": "pi@749",
            "to": ["@730"],
            "body": body,
            "created_at": "2026-10-18T00:00:00Z",
            "read_at": null,
            "acked_at": null,
        }))
        .unwrap()
    }

    #[test]
    fn the_doorbell_is_one_line_naming_sender_thread_and_the_way_back() {
        let line = doorbell(&mail("rebased #1622\nsecond line\twith a tab"));
        assert!(!line.contains('\n') && !line.contains('\t'), "{line}");
        assert!(
            line.contains("pi@749") && line.contains("desktop-ux"),
            "{line}"
        );
        assert!(
            line.contains("rebased #1622…"),
            "a longer body is elided: {line}"
        );
        assert!(
            line.contains("phux mail read --thread desktop-ux"),
            "{line}"
        );
        let long = doorbell(&mail(&"x".repeat(500)));
        assert!(long.chars().count() < 400, "{long}");
    }

    #[test]
    fn claimed_paths_are_absolute_and_normalized_without_touching_disk() {
        assert_eq!(absolute("/a/./b/../c/").unwrap(), "/a/c");
        assert!(absolute("rel").unwrap().starts_with('/'));
    }

    #[test]
    fn native_and_legacy_terminal_context_keep_their_pane_identity() {
        for id in [730, 749] {
            let native = ResourceId::local(id).to_string();
            let expected = format!("@{id}");
            assert_eq!(terminal_address(&native), expected);
            assert_eq!(terminal_address(&id.to_string()), expected);
            assert_eq!(
                Address::parse(&terminal_address(&native)).unwrap().key(),
                expected
            );
        }
        assert_ne!(terminal_address("@730"), terminal_address("@749"));
        assert!(Address::parse(&terminal_address("malformed")).is_err());
    }

    #[test]
    fn a_pane_speaks_as_itself_unless_told_otherwise() {
        let me = AsOpt {
            me: Some("pi@749".into()),
        };
        assert_eq!(whoami(&me), "pi@749");
    }
}
