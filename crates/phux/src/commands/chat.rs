//! Explicit conversations over the existing mail-only hub connection.
//!
//! A conversation pins its own enrolled identity and inbox. It never changes
//! the ordinary mail/claim home or exposes either participant's terminals.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::conversation::{self, Conversation};
use usage::{Args, Subcommands};

use super::json_err::{self, CliError, codes};

mod invite;
pub(crate) mod messages;
mod terminal;

const SCHEMA: &str = "phux.chat/v1";

/// Talk with a friend or agent over native phux mail
///
/// Requires an already reachable, paired phux hub. One person creates an
/// invite; the other joins by pasting it. Messages stay in that hub's durable
/// mail store. The hub can read them; neither participant gets terminal
/// access. Local mail, claims and existing hub connections are unchanged.
#[derive(Debug, Args)]
pub(crate) struct ChatArgs {
    #[usage(subcommand)]
    pub(crate) action: ChatAction,
}

#[derive(Debug, Subcommands)]
pub(crate) enum ChatAction {
    /// Create a single-use messaging invite and store a welcome message
    Invite {
        /// A new local name for this conversation.
        name: String,
        /// Address the friend can reach. Defaults to the live QUIC listener.
        #[usage(long, value_name = "HOST:PORT")]
        hub: Option<String>,
        /// The first real message the joining peer receives.
        #[usage(
            short = 'm',
            long,
            default = "Hello! Welcome to our phux conversation."
        )]
        message: String,
        /// Minutes the invitation remains redeemable.
        #[usage(long = "expires-in", default = "10", value_name = "MINUTES")]
        expires_in: i64,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Paste and redeem a messaging invite; receive the welcome and confirm it
    ///
    /// Reads one invite from stdin, keeping its secret out of process
    /// arguments and shell history. Re-run after a partial connection failure
    /// with the same local name to resume an identity already saved here.
    Join {
        /// A new local name; it need not match the inviter's name.
        name: String,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Store a message for the other participant
    Send {
        #[usage(flatten)]
        target: ChatTarget,
        /// Message text; `-` reads bounded text from stdin.
        #[usage(short = 'm', long, value_name = "TEXT")]
        message: String,
        /// Retry an uncertain send with the same id and identical text.
        #[usage(long, value_name = "HEX32")]
        operation_id: Option<String>,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Show conversation history without consuming agent delivery
    Read {
        #[usage(flatten)]
        target: ChatTarget,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Check the actual mailbox and whether the peer confirmed the welcome
    Status {
        #[usage(flatten)]
        target: ChatTarget,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Open a line-oriented conversation; enter /quit to leave the view
    ///
    /// Reads do not take or mark mail read. The newest 500 messages remain
    /// visible through the existing mail history interface.
    Open {
        #[usage(flatten)]
        target: ChatTarget,
    },
    /// Run an agent with only its mail scoped to this conversation
    ///
    /// Requires that harness's phux integration. pi, Oh My Pi and `OpenCode`
    /// can wake an idle conversation. Claude and Grok receive at their next
    /// hook. Claims and terminal permissions are not redirected.
    Run {
        #[usage(flatten)]
        target: ChatTarget,
        /// Program and arguments, after `--`.
        #[usage(required, trailing_var_arg, value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// Acknowledge a received message in this conversation
    Ack {
        #[usage(flatten)]
        target: ChatTarget,
        /// The message id printed by read.
        #[usage(long, value_name = "HEX32")]
        id: String,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
}

#[derive(Debug, Args)]
pub(crate) struct ChatTarget {
    /// The local conversation name.
    name: Option<String>,
    /// Exact private record; independent of HOME, profile and alias.
    #[usage(long, value_name = "PATH")]
    record: Option<PathBuf>,
}

impl ChatTarget {
    fn resolve(&self) -> Result<(PathBuf, Conversation), String> {
        match (&self.name, &self.record) {
            (Some(_), Some(_)) => Err("pass a conversation name or --record, not both".to_owned()),
            (Some(name), None) => {
                let path = conversation::directory(name)?.join("conversation.json");
                let chat = conversation::load_path(&path)?;
                Ok((path, chat))
            }
            (None, Some(path)) => {
                let chat = conversation::load_path(path)?;
                Ok((path.clone(), chat))
            }
            (None, None) => Err("name a conversation, or pass --record PATH".to_owned()),
        }
    }
}

pub(crate) fn run(args: ChatArgs, socket: Option<PathBuf>) -> ExitCode {
    // An older phux made it 0755, which conversations refuse; a refusal below
    // reports what this could not repair.
    let _ = phux_server::owner_dir::tighten(&phux_config::instance::state_dir());
    match args.action {
        ChatAction::Invite {
            name,
            hub,
            message,
            expires_in,
            json,
        } => invite::create(
            &name,
            hub.as_deref(),
            &message,
            expires_in,
            json.json,
            socket,
        ),
        ChatAction::Join { name, json } => invite::join(&name, json.json),
        ChatAction::Send {
            target,
            message,
            operation_id,
            json,
        } => messages::send(&target, &message, operation_id.as_deref(), json.json),
        ChatAction::Read { target, json } => messages::read(&target, json.json),
        ChatAction::Status { target, json } => messages::status(&target, json.json),
        ChatAction::Open { target } => terminal::open(&target),
        ChatAction::Run { target, command } => messages::run_agent(&target, &command),
        ChatAction::Ack { target, id, json } => messages::ack(&target, &id, json.json),
    }
}

fn failure(json: bool, message: impl Into<String>, remedy: impl Into<String>) -> ExitCode {
    json_err::emit(json, &CliError::new(codes::WORKLOAD, message, remedy), 1)
}

fn emit(value: &serde_json::Value) -> ExitCode {
    let mut value = value.clone();
    value["schema"] = SCHEMA.into();
    value["schema_version"] = 1.into();
    crate::output::json(&value)
}

/// Remote text is data, never terminal control sequences.
fn display_text(text: &str) -> String {
    text.chars().filter(|c| safe_character(*c)).collect()
}

const fn safe_character(c: char) -> bool {
    if c == '\n' || c == '\t' {
        return true;
    }
    !c.is_control()
        && !matches!(c, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}' | '\u{feff}' | '\u{e0000}'..='\u{e007f}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_text_cannot_emit_terminal_or_direction_controls() {
        let text = "hi\x1b]52;c;secret\x07\r\u{202e}there\nnext\tline";
        let rendered = display_text(text);
        assert!(!rendered.contains('\x1b'));
        assert!(!rendered.contains('\x07'));
        assert!(!rendered.contains('\r'));
        assert!(!rendered.contains('\u{202e}'));
        assert!(rendered.contains("\nnext\tline"));
    }
}
