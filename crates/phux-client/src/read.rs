//! Kind dispatch for the verbs that address one resource.
//!
//! `phux snapshot` and `phux wait` read a document by its kind; `phux
//! send-keys` and every other verb that reaches a Terminal only (`run`,
//! `paste`, `resize`, `upload`, `take`, `give`, `signal`, `ask`, `rec`, and
//! the agent input verbs) refuse any other kind.
//!
//! Their MCP tools route through it too, but read a Terminal only, so they keep their
//! read-only annotation and name the per-kind tool for a document.
//!
//! The kind catalog ([`phux_protocol::kinds`]) decides. Each verb names the
//! Terminal facet method it sends ([`Verb::terminal_method`]); a kind whose
//! facet carries it takes the verb: today the Terminal. The document kinds
//! are read through an attach ([`crate::tree`]); their input goes to their
//! producer as typed intents, which keys do not map onto, so the input
//! verbs refuse them. Any other kind, or one this build does not know, is
//! refused with [`WrongKind`] before anything is sent.

use std::borrow::Cow;
use std::path::Path;

use phux_protocol::ids::{ResourceId, ResourceKind};
use phux_protocol::kinds;
use phux_protocol::wire::frame::typed_document::is_textual;
use serde_json::{Value, json};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::deadline::Deadline;
use crate::selector::format_terminal_id;
use crate::tree::{self, Document, FollowEnd, Kind, TreeError, image_size};
use crate::wait::{Condition, MatchRegex};

pub use crate::selector::Resolved;

/// `schema_version` of the documents this module builds.
pub const SCHEMA_VERSION: u8 = 1;

/// A verb that dispatches on its target's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// Read the resource once.
    Snapshot,
    /// Block until its content matches.
    Wait,
    /// Type keys into it.
    SendKeys,
    /// Paste text into it.
    Paste,
    /// Run a shell command in it.
    Run,
    /// Set its grid.
    Resize,
    /// Put a file beside its process.
    Upload,
    /// Seize its input lease.
    Take,
    /// Release its input lease.
    Give,
    /// Signal its process group.
    Signal,
    /// Report that its agent is asking.
    Ask,
    /// Record its screen.
    Rec,
    /// `phux agent send-keys`.
    AgentSendKeys,
    /// `phux agent prompt`.
    AgentPrompt,
    /// `phux agent answer`.
    AgentAnswer,
    /// `phux agent start`.
    AgentStart,
    /// `phux agent report-state`.
    AgentReportState,
}

impl Verb {
    /// Every verb, for the tests that walk the catalog.
    pub const ALL: [Self; 17] = [
        Self::Snapshot,
        Self::Wait,
        Self::SendKeys,
        Self::Paste,
        Self::Run,
        Self::Resize,
        Self::Upload,
        Self::Take,
        Self::Give,
        Self::Signal,
        Self::Ask,
        Self::Rec,
        Self::AgentSendKeys,
        Self::AgentPrompt,
        Self::AgentAnswer,
        Self::AgentStart,
        Self::AgentReportState,
    ];

    /// The CLI verb's name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::Wait => "wait",
            Self::SendKeys => "send-keys",
            Self::Paste => "paste",
            Self::Run => "run",
            Self::Resize => "resize",
            Self::Upload => "upload",
            Self::Take => "take",
            Self::Give => "give",
            Self::Signal => "signal",
            Self::Ask => "ask",
            Self::Rec => "rec",
            Self::AgentSendKeys => "agent send-keys",
            Self::AgentPrompt => "agent prompt",
            Self::AgentAnswer => "agent answer",
            Self::AgentStart => "agent start",
            Self::AgentReportState => "agent report-state",
        }
    }

    /// The Terminal facet method the verb sends, as the kind catalog names
    /// it. `rec` attaches the output stream, which every kind has; it
    /// records a screen, so it needs the kind that has one.
    #[must_use]
    pub const fn terminal_method(self) -> &'static str {
        match self {
            Self::Snapshot | Self::Wait | Self::Rec => "GET_SCREEN",
            Self::SendKeys | Self::Paste | Self::Run => "ROUTE_INPUT",
            Self::AgentSendKeys | Self::AgentPrompt | Self::AgentAnswer | Self::AgentStart => {
                "APPLY_INPUT"
            }
            Self::Resize => "RESIZE_TERMINAL",
            Self::Upload => "PUT_FILE",
            Self::Take => "ACQUIRE_INPUT",
            Self::Give => "RELEASE_INPUT",
            Self::Signal => "SIGNAL_TERMINAL",
            Self::Ask => "REPORT_ASKED",
            Self::AgentReportState => "REPORT_AGENT_STATE",
        }
    }

    /// Whether the verb types text a document's producer takes as typed
    /// intents. `run` and `agent start` type a shell command line, which a
    /// document has no shell for.
    const fn types(self) -> bool {
        matches!(
            self,
            Self::SendKeys
                | Self::Paste
                | Self::AgentSendKeys
                | Self::AgentPrompt
                | Self::AgentAnswer
        )
    }
}

/// How a verb reaches its resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The Terminal facet: `GET_SCREEN` to read, `ROUTE_INPUT` to type.
    Terminal,
    /// A document kind, read through an attach.
    Document(Kind),
}

/// A verb that does not apply to its resource's kind.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{}` does not apply to {}, {}", verb.name(), format_terminal_id(resource), kind_label(*kind))]
pub struct WrongKind {
    /// The resource named.
    pub resource: ResourceId,
    /// Its kind.
    pub kind: ResourceKind,
    /// The verb refused.
    pub verb: Verb,
}

impl WrongKind {
    /// What to do instead.
    #[must_use]
    pub fn remedy(&self) -> &'static str {
        if kinds::kind_spec(self.kind).is_none() {
            return "this phux does not know the resource's kind; update phux";
        }
        match (self.verb, document_kind(self.kind)) {
            (verb, Some(_)) if verb.types() => {
                "a document takes input through its producer as typed intents: \
                 `phux agent page type TEXT` or `phux agent page key KEY` \
                 (MCP phux_document_type, phux_document_key)"
            }
            (Verb::Wait, Some(Kind::Surface)) => {
                "a surface is pixels, not text: read it with `phux snapshot TARGET --out FILE`, \
                 or wait on the accessibility tree beside it"
            }
            _ if self.kind == ResourceKind::AgentSession => {
                "read an agent session with `phux agent log`; address its parent pane \
                 for the screen and for keys"
            }
            _ => "address a Terminal; `phux ls --json` lists each resource's kind",
        }
    }
}

/// An option that applies to a Terminal only, given for another kind.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{option}` applies to a terminal only, and {} is {}", format_terminal_id(resource), kind_label(*kind))]
pub struct TerminalOnly {
    /// The resource named.
    pub resource: ResourceId,
    /// Its kind.
    pub kind: ResourceKind,
    /// The option, as the caller spells it (`--tail`, `cells`).
    pub option: &'static str,
}

/// How `verb` reaches `resolved`, read from the kind catalog.
///
/// # Errors
///
/// [`WrongKind`] when the verb does not apply to the kind: an
/// `AgentSession`, a kind this build does not know, a document for any verb
/// but `snapshot` and `wait`, or a pixel surface for `wait`, which has no
/// text.
pub fn route(verb: Verb, resolved: &Resolved) -> Result<Route, WrongKind> {
    let refuse = || wrong_kind(verb, resolved);
    let spec = kinds::kind_spec(resolved.kind).ok_or_else(refuse)?;
    if spec
        .methods
        .iter()
        .any(|method| method.name == verb.terminal_method())
    {
        return Ok(Route::Terminal);
    }
    match (verb, document_kind(resolved.kind)) {
        (Verb::Snapshot, Some(kind)) | (Verb::Wait, Some(kind @ (Kind::Tree | Kind::Document))) => {
            Ok(Route::Document(kind))
        }
        _ => Err(refuse()),
    }
}

/// [`route`] for a verb that reaches a Terminal only: `Ok` when `resolved`
/// takes it on the Terminal facet. Read from the snapshot the target was
/// resolved against, so a Terminal costs no round trip.
///
/// # Errors
///
/// [`WrongKind`] for any other kind, a document included.
pub fn require_terminal(verb: Verb, resolved: &Resolved) -> Result<(), WrongKind> {
    match route(verb, resolved)? {
        Route::Terminal => Ok(()),
        Route::Document(_) => Err(wrong_kind(verb, resolved)),
    }
}

fn wrong_kind(verb: Verb, resolved: &Resolved) -> WrongKind {
    WrongKind {
        resource: resolved.id.clone(),
        kind: resolved.kind,
        verb,
    }
}

/// The first of `options` that is set, refused for a resource that is not
/// read as a screen. `options` pairs each option's spelling with whether
/// the caller gave it.
///
/// # Errors
///
/// [`TerminalOnly`] naming that option.
pub fn terminal_only(
    resolved: &Resolved,
    route: Route,
    options: &[(&'static str, bool)],
) -> Result<(), TerminalOnly> {
    if route == Route::Terminal {
        return Ok(());
    }
    options
        .iter()
        .find(|(_, given)| *given)
        .map_or(Ok(()), |(option, _)| {
            Err(TerminalOnly {
                resource: resolved.id.clone(),
                kind: resolved.kind,
                option,
            })
        })
}

/// The document kind `kind` names, if it is one.
const fn document_kind(kind: ResourceKind) -> Option<Kind> {
    match kind {
        ResourceKind::AccessibilityTree => Some(Kind::Tree),
        ResourceKind::PixelSurface => Some(Kind::Surface),
        ResourceKind::Document => Some(Kind::Document),
        _ => None,
    }
}

/// How a message names `kind`, with its article: `an accessibility tree`.
#[must_use]
pub fn kind_label(kind: ResourceKind) -> String {
    match kind {
        ResourceKind::Terminal => "a terminal".to_owned(),
        ResourceKind::AgentSession => "an agent session".to_owned(),
        ResourceKind::AccessibilityTree => "an accessibility tree".to_owned(),
        ResourceKind::PixelSurface => "a pixel surface".to_owned(),
        ResourceKind::Document => "a document".to_owned(),
        other => format!("a resource of kind {other}"),
    }
}

/// The text of a document of `kind`: a tree's JSON, or a typed document
/// whose content type is text. `None` for a surface or a binary document.
#[must_use]
pub fn text_of(kind: Kind, document: &Document) -> Option<Cow<'_, str>> {
    let textual = match kind {
        Kind::Tree => true,
        Kind::Surface => false,
        Kind::Document => document.content_type.as_deref().is_some_and(is_textual),
    };
    textual.then(|| String::from_utf8_lossy(&document.bytes))
}

/// The newest document of `resource`, over a fresh connection.
///
/// # Errors
///
/// The refusal, or a connection failure.
pub async fn document(socket: &Path, resource: &ResourceId) -> Result<Document, TreeError> {
    let mut conn = Connection::connect(socket).await?;
    tree::snapshot(&mut conn, resource).await
}

/// A typed document as `phux snapshot --json` prints it: the facts
/// `phux agent doc snapshot --json` prints, and its `text` when the content
/// type is text.
#[must_use]
pub fn document_json(document: &Document) -> Value {
    let size = image_size(&document.bytes).ok();
    json!({
        "schema_version": SCHEMA_VERSION,
        "seq": document.seq,
        "bytes": document.bytes.len(),
        "content_type": document.content_type,
        "format": size.map(|s| s.0.name()),
        "width": size.map(|s| s.1),
        "height": size.map(|s| s.2),
        "text": text_of(Kind::Document, document),
    })
}

/// What a document wait matches: a wait's text condition.
#[derive(Debug, Clone, Copy)]
pub enum TextMatch<'a> {
    /// The text contains this substring, as `phux agent tree follow --until`.
    Contains(&'a str),
    /// Some line of the text matches this pattern.
    Regex(&'a MatchRegex),
}

impl<'a> TextMatch<'a> {
    /// The text condition `condition` names; `None` for an idle wait, which
    /// reads a screen.
    #[must_use]
    pub const fn of(condition: &'a Condition) -> Option<Self> {
        match condition {
            Condition::Contains(needle) => Some(Self::Contains(needle.as_str())),
            Condition::Matches(pattern) => Some(Self::Regex(pattern)),
            Condition::Idle(_) => None,
        }
    }

    /// Whether `text` meets it.
    #[must_use]
    pub fn is_match(&self, text: &str) -> bool {
        match self {
            Self::Contains(needle) => text.contains(needle),
            Self::Regex(pattern) => text.lines().any(|line| pattern.is_match(line)),
        }
    }
}

/// How a document wait ended. `reads` counts the documents examined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentWait {
    /// A document matched.
    Matched {
        /// The document that matched.
        document: Document,
        /// Documents examined.
        reads: u64,
    },
    /// The resource closed first.
    Closed {
        /// Documents examined.
        reads: u64,
    },
    /// The deadline passed first.
    TimedOut {
        /// Documents examined.
        reads: u64,
    },
}

/// Why a document wait could not run.
#[derive(Debug, thiserror::Error)]
pub enum WaitError {
    /// The document is not text, so no text condition can hold.
    #[error(
        "{} is a {content_type} document, not text; a wait matches text",
        format_terminal_id(resource)
    )]
    NotText {
        /// The document.
        resource: ResourceId,
        /// Its content type.
        content_type: String,
    },
    /// The read failed.
    #[error(transparent)]
    Tree(#[from] TreeError),
}

/// Follow `resource`, a document of `kind`, until a document's text meets
/// `matcher`, it closes, or `deadline` passes.
///
/// # Errors
///
/// [`WaitError::NotText`] for a binary document, or the read's failure; a
/// dropped connection is [`AttachError::Disconnected`].
pub async fn wait_document(
    socket: &Path,
    resource: &ResourceId,
    kind: Kind,
    matcher: TextMatch<'_>,
    deadline: Deadline,
) -> Result<DocumentWait, WaitError> {
    let mut reads = 0;
    let mut outcome: Option<Result<Document, WaitError>> = None;
    let followed = deadline
        .run(async {
            let mut conn = Connection::connect(socket).await?;
            tree::follow(&mut conn, resource, |document| {
                reads += 1;
                let verdict = match text_of(kind, &document) {
                    None => Some(Err(WaitError::NotText {
                        resource: resource.clone(),
                        content_type: document.content_type.clone().unwrap_or_default(),
                    })),
                    Some(text) if matcher.is_match(&text) => Some(Ok(())),
                    Some(_) => None,
                };
                outcome = verdict.map(|verdict| verdict.map(|()| document));
                outcome.is_none()
            })
            .await
        })
        .await;
    match (followed, outcome) {
        (_, Some(Ok(document))) => Ok(DocumentWait::Matched { document, reads }),
        (_, Some(Err(err))) => Err(err),
        (None, None) => Ok(DocumentWait::TimedOut { reads }),
        (Some(Err(err)), None) => Err(err.into()),
        (Some(Ok(FollowEnd::Disconnected)), None) => {
            Err(TreeError::Protocol(AttachError::Disconnected).into())
        }
        (Some(Ok(FollowEnd::Closed | FollowEnd::Stopped)), None) => {
            Ok(DocumentWait::Closed { reads })
        }
    }
}

#[cfg(test)]
mod tests;
