//! `phux mail` and `phux claim`: agent coordination in the work store
//! (ADR-0187).
//!
//! Both talk to the work store's keeper, which `phux mission` also uses and
//! starts on demand. `mail send` only stores the message. Nothing is ever
//! typed into a recipient's terminal: its harness takes the mail with
//! `mail wait` or `mail hook-context` and puts it in the agent's
//! conversation through the harness's own API ([`delivery`]).
//!
//! Who you are: `--as`, else `$PHUX_AGENT_ADDRESS`, else this pane (`@N`
//! from `$PHUX_TERMINAL_ID`), else `$USER`.
//!
//! On a host joined to a hub with `phux host join`, both use the hub's store
//! instead, in this host's name ([`phux_client::hub_mail`]).

use std::process::ExitCode;

use phux_work::{Claim, ClaimFilter, ClaimOutcome, ReleaseClaim, RenewClaim, TakeClaim};

use super::json_err::{self, codes};

mod delivery;
mod mail;
use delivery::HookEvent;
use mail::{ack, read, send};

/// Schema of every `phux mail` and `phux claim` JSON document.
const SCHEMA: &str = "phux.coord/v1";
/// The `schema_version` every coordination document carries beside `schema`,
/// as every other `--json` document does.
pub(crate) const SCHEMA_VERSION: u8 = 1;

#[derive(Debug, usage::Subcommands)]
pub(crate) enum MailAction {
    /// Store a message for each recipient
    Send {
        /// Recipients: a pane (`@749`), a labelled pane (`pi@749`), or a name,
        /// led by `SATELLITE/` or `hub/` for one on another host.
        #[usage(required, value_name = "TO")]
        to: Vec<String>,
        /// The message. `-` reads it from stdin.
        #[usage(short = 'm', long, value_name = "TEXT")]
        message: String,
        /// Thread slug, such as the branch or task the message is about.
        #[usage(short = 't', long, value_name = "SLUG")]
        thread: String,
        /// Durable retry identity: nonzero 32-hex. Reuse only with identical sender, recipients, thread and body in the same work store.
        #[usage(long, value_name = "KEY")]
        operation_id: Option<String>,
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
    /// Wait for unread mail and take it for your agent's conversation
    ///
    /// For harness integrations. Returns as soon as there is mail neither
    /// read nor taken before, or at the deadline with none. What it returns
    /// is taken: no later `wait` or `hook-context` for this address returns
    /// it again, while `read` still shows it unread. `text` is the mail
    /// framed for the agent, or null when there is none.
    Wait {
        /// Seconds to wait when there is no mail; 0 answers at once. Default 300, at most 3600.
        #[usage(long, value_name = "SECS")]
        timeout: Option<u64>,
        /// At most this many messages, oldest first. Default and most 16.
        #[usage(long, value_name = "N")]
        limit: Option<u32>,
        /// Give up, taking nothing, once stdin closes. A harness holds stdin
        /// open for its whole life, so a wait never outlives it.
        #[usage(long)]
        until_stdin_closes: bool,
        #[usage(flatten)]
        me: AsOpt,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Return taken mail so the next wait takes it again
    ///
    /// For harness integrations whose injection failed after `wait` took
    /// the mail. Ids this address did not take are ignored.
    Release {
        /// Message ids, as `wait` returned them.
        #[usage(required, value_name = "ID")]
        ids: Vec<String>,
        #[usage(flatten)]
        me: AsOpt,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Take unread mail and print it as a Claude Code or Grok hook reply
    ///
    /// For hook-only harnesses. Prints nothing when there is no mail.
    /// Otherwise takes up to 4 messages, like `wait --timeout 0`, and prints
    /// one hook JSON object: `additionalContext` for a context event, or a
    /// `block` decision whose reason is the mail for `stop`, so the agent
    /// keeps going and handles it.
    #[usage(name = "hook-context")]
    HookContext {
        /// The hook event the reply answers.
        #[usage(long, value_enum, value_name = "EVENT")]
        event: HookEvent,
        #[usage(flatten)]
        me: AsOpt,
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

/// Who this process speaks as ([`phux_work::caller_address`]).
fn whoami(me: &AsOpt) -> String {
    phux_work::caller_address(me.me.as_deref())
}

pub(crate) fn run_mail(action: &MailAction) -> ExitCode {
    match action {
        MailAction::Send {
            to,
            message,
            thread,
            operation_id,
            me,
            json,
        } => send(
            to,
            message,
            thread,
            &whoami(me),
            json.json,
            operation_id.as_deref(),
        ),
        MailAction::Read {
            thread,
            all,
            peek,
            me,
            json,
        } => read(thread.clone(), *all, *peek, &whoami(me), json.json),
        MailAction::Ack { id, me, json } => ack(id, &whoami(me), json.json),
        MailAction::Wait {
            timeout,
            limit,
            until_stdin_closes,
            me,
            json,
        } => delivery::wait(
            delivery::WaitArgs {
                timeout: *timeout,
                limit: *limit,
                until_stdin_closes: *until_stdin_closes,
            },
            &whoami(me),
            json.json,
        ),
        MailAction::Release { ids, me, json } => delivery::release(ids, &whoami(me), json.json),
        MailAction::HookContext { event, me } => delivery::hook_context(*event, &whoami(me)),
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

/// The hub's store when this host joined one, else this host's.
fn store(json: bool) -> Result<phux_work::Client, ExitCode> {
    match phux_client::hub_mail::hub_link() {
        Ok(Some(link)) => Ok(phux_work::Client::remote(std::sync::Arc::new(link))),
        Ok(None) => super::mission::client(json),
        Err(message) => Err(json_err::emit(
            json,
            &json_err::CliError::new(
                codes::INVALID_CONFIG,
                message,
                "fix the [[uplink]] entry `phux host join` wrote, or join again",
            ),
            crate::exit_codes::EXIT_USAGE,
        )),
    }
}

fn emit(document: &serde_json::Value) {
    let mut document = document.clone();
    document["schema_version"] = SCHEMA_VERSION.into();
    match serde_json::to_string(&document) {
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
    phux_work::claim_path(&joined).ok_or_else(|| format!("{} is not UTF-8", joined.display()))
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
        ttl_secs: ttl.unwrap_or(phux_work::DEFAULT_CLAIM_SECS),
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
        ttl_secs: ttl.unwrap_or(phux_work::DEFAULT_CLAIM_SECS),
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
    use phux_protocol::ResourceId;
    use phux_work::{Address, pane_address};

    use super::*;

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
            assert_eq!(pane_address(&native), expected);
            assert_eq!(pane_address(&id.to_string()), expected);
            assert_eq!(
                Address::parse(&pane_address(&native)).unwrap().key(),
                expected
            );
        }
        assert_ne!(pane_address("@730"), pane_address("@749"));
        assert!(Address::parse(&pane_address("malformed")).is_err());
    }

    #[test]
    fn a_pane_speaks_as_itself_unless_told_otherwise() {
        let me = AsOpt {
            me: Some("pi@749".into()),
        };
        assert_eq!(whoami(&me), "pi@749");
    }
}
