use std::ffi::c_void;
use std::ptr;

use phux_protocol::ResourceId;

pub const ABI_VERSION: u32 = 2;
pub const CELL_BOLD: u32 = 1 << 0;
pub const CELL_ITALIC: u32 = 1 << 1;
pub const CELL_FAINT: u32 = 1 << 2;
pub const CELL_BLINK: u32 = 1 << 3;
pub const CELL_INVERSE: u32 = 1 << 4;
pub const CELL_INVISIBLE: u32 = 1 << 5;
pub const CELL_STRIKETHROUGH: u32 = 1 << 6;
pub const CELL_OVERLINE: u32 = 1 << 7;
pub const CELL_SELECTED: u32 = 1 << 8;
pub const CELL_PROTECTED: u32 = 1 << 9;
pub const CELL_HYPERLINK: u32 = 1 << 10;

/// `status_code` on a `DETACHED` status effect when the server stated no
/// reason — an older server, or a `DetachReason` this build does not
/// recognise (`docs/spec/proto.md` §7.2).
///
/// Deliberately outside the one-byte wire space rather than `0`: `REQUESTED`
/// *is* `0`, so a zero default would report "the user asked to detach" for
/// every ending nobody asked for.
pub const DETACH_REASON_UNSTATED: u32 = 0xFFFF;

/// `PhuxClientEffect.kind` carrying `AgentEventsJsonlV1` records.
///
/// One effect holds one or more complete records from an `AgentSession`
/// resource stream. The bridge never rewrites or splits records; `bytes` is
/// the wire payload byte-for-byte.
pub const EFFECT_AGENT_RECORDS: u32 = 4;
/// `detail` on an `AGENT_RECORDS` effect: the complete retained backlog of a
/// stream generation, published once at `BOOTSTRAP_READY`.
pub const AGENT_RECORDS_RETAINED: u32 = 1;
/// `detail` on an `AGENT_RECORDS` effect: records from one live output frame.
pub const AGENT_RECORDS_LIVE: u32 = 2;
/// `detail` on an `AGENT_RECORDS` effect: the resource closed; `bytes` is
/// empty and the resource leaves the resource list.
pub const AGENT_RECORDS_CLOSED: u32 = 3;
/// `PhuxResourceInfo.kind` wire tag for a PTY-backed Terminal resource.
pub const RESOURCE_KIND_TERMINAL: u32 = 0;
/// `PhuxResourceInfo.kind` wire tag for an `AgentSession` resource.
pub const RESOURCE_KIND_AGENT_SESSION: u32 = 1;

#[repr(i32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhuxClientResult {
    Ok = 0,
    NoValue = 1,
    InvalidArgument = -1,
    InvalidState = -2,
    ProtocolError = -3,
    EngineError = -4,
    OutOfMemory = -5,
    Panic = -6,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhuxClientState {
    New = 0,
    HelloQueued = 1,
    Negotiated = 2,
    Attached = 3,
    Detached = 4,
    Failed = 5,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxBytes {
    pub data: *const u8,
    pub len: usize,
}

impl Default for PhuxBytes {
    fn default() -> Self {
        Self {
            data: ptr::null(),
            len: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PhuxResourceId {
    pub kind: u32,
    pub id: u32,
    pub host: PhuxBytes,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PhuxSessionInfo {
    pub session_id: u32,
    pub name: PhuxBytes,
    pub created_at_unix_secs: i64,
    pub window_count: u16,
    pub attached_client_count: u16,
    pub focused: bool,
}

/// Borrowed resource summary from the latest `ATTACHED` snapshot.
///
/// `parent` is null when the resource has no parent; otherwise it points at
/// bridge-owned storage valid until the next mutable call, like every other
/// borrowed span. `provider`, `native_id`, and `state` are empty for kinds
/// without an agent facet.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxResourceInfo {
    pub size: usize,
    pub version: u32,
    pub terminal_id: PhuxResourceId,
    pub kind: u32,
    pub parent: *const PhuxResourceId,
    pub provider: PhuxBytes,
    pub native_id: PhuxBytes,
    pub state: PhuxBytes,
}

impl Default for PhuxResourceInfo {
    fn default() -> Self {
        Self {
            size: std::mem::size_of::<Self>(),
            version: ABI_VERSION,
            terminal_id: PhuxResourceId::default(),
            kind: RESOURCE_KIND_TERMINAL,
            parent: ptr::null(),
            provider: PhuxBytes::default(),
            native_id: PhuxBytes::default(),
            state: PhuxBytes::default(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxClientOptions {
    pub size: usize,
    pub version: u32,
    pub max_bootstrap_chunk_bytes: u32,
    pub max_history_page_bytes: u32,
    pub max_history_page_rows: u32,
    pub max_history_cache_bytes: usize,
    pub max_history_materialized_rows: usize,
    pub history_prefetch_rows: usize,
}
pub type PhuxClientAttachedCallback = unsafe extern "C-unwind" fn(userdata: *mut c_void);
pub type PhuxClientFailureCallback = unsafe extern "C-unwind" fn(
    userdata: *mut c_void,
    result: PhuxClientResult,
    message: PhuxBytes,
);

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxClientCallbacks {
    pub size: usize,
    pub version: u32,
    pub userdata: *mut c_void,
    pub on_attached: Option<PhuxClientAttachedCallback>,
    pub on_failure: Option<PhuxClientFailureCallback>,
}

impl Default for PhuxClientCallbacks {
    fn default() -> Self {
        Self {
            size: std::mem::size_of::<Self>(),
            version: ABI_VERSION,
            userdata: ptr::null_mut(),
            on_attached: None,
            on_failure: None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxAttachOptions {
    pub size: usize,
    pub version: u32,
    pub attach_id: u32,
    pub target_kind: u32,
    pub session_id: u32,
    pub name: PhuxBytes,
    pub cols: u16,
    pub rows: u16,
    pub has_pixel_size: bool,
    pub pixel_width: u16,
    pub pixel_height: u16,
    pub request_scrollback: bool,
    pub scrollback_limit_lines: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PhuxClientEffect {
    pub kind: u32,
    pub detail: u32,
    pub status_code: u32,
    pub terminal_id: PhuxResourceId,
    pub stream_id: u64,
    pub bootstrap_id: u64,
    pub seq: u64,
    pub first_row: u16,
    pub last_row: u16,
    pub bytes: PhuxBytes,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PhuxDocumentAnchor {
    pub opaque_id: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PhuxDocumentPoint {
    pub space: u32,
    pub row: u32,
    pub column: u16,
    pub reserved: u16,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PhuxTerminalCell {
    pub utf8_offset: u32,
    pub utf8_len: u16,
    pub content_tag: u16,
    pub hyperlink_offset: u32,
    pub hyperlink_len: u32,
    pub wide: u8,
    pub semantic_content: u8,
    pub flags: u32,
    pub foreground_r: u8,
    pub foreground_g: u8,
    pub foreground_b: u8,
    pub background_r: u8,
    pub background_g: u8,
    pub background_b: u8,
    pub underline: u8,
    pub underline_r: u8,
    pub underline_g: u8,
    pub underline_b: u8,
    pub reserved: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxTerminalGridView {
    pub terminal_id: PhuxResourceId,
    pub stream_id: u64,
    pub bootstrap_id: u64,
    pub last_seq: u64,
    pub document_revision: u64,
    pub cols: u16,
    pub rows: u16,
    pub cells: *const PhuxTerminalCell,
    pub cell_count: usize,
    pub utf8: PhuxBytes,
    pub cursor_visible: bool,
    pub cursor_col: u16,
    pub cursor_row: u16,
    pub cursor_style: u32,
    pub history_total_rows: u64,
    pub history_viewport_offset: u64,
    pub history_visible_rows: u64,
    pub history_pages_loaded: u64,
    pub history_unread_rows: u64,
    pub history_bytes_loaded: u64,
    pub history_loading: bool,
    pub history_has_more: bool,
    pub top_anchor: PhuxDocumentAnchor,
}

impl Default for PhuxTerminalGridView {
    fn default() -> Self {
        Self {
            terminal_id: PhuxResourceId::default(),
            stream_id: 0,
            bootstrap_id: 0,
            last_seq: 0,
            document_revision: 0,
            cols: 0,
            rows: 0,
            cells: ptr::null(),
            cell_count: 0,
            utf8: PhuxBytes::default(),
            cursor_visible: false,
            cursor_col: 0,
            cursor_row: 0,
            cursor_style: 0,
            history_total_rows: 0,
            history_viewport_offset: 0,
            history_visible_rows: 0,
            history_pages_loaded: 0,
            history_unread_rows: 0,
            history_bytes_loaded: 0,
            history_loading: false,
            history_has_more: false,
            top_anchor: PhuxDocumentAnchor::default(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxKeyEvent {
    pub size: usize,
    pub version: u32,
    pub action: u32,
    pub key: u32,
    pub modifiers: u16,
    pub consumed_modifiers: u16,
    pub composing: bool,
    pub has_text: bool,
    pub text: PhuxBytes,
    pub has_unshifted_codepoint: bool,
    pub unshifted_codepoint: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxMouseEvent {
    pub size: usize,
    pub version: u32,
    pub action: u32,
    pub button: u32,
    pub modifiers: u16,
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PhuxSearchResult {
    pub start: PhuxDocumentAnchor,
    pub end: PhuxDocumentAnchor,
}

#[derive(Debug)]
pub struct OwnedEffect {
    pub status_code: u32,
    pub kind: u32,
    pub detail: u32,
    pub terminal_id: ResourceId,
    pub stream_id: u64,
    pub bootstrap_id: u64,
    pub seq: u64,
    pub first_row: u16,
    pub last_row: u16,
    pub bytes: Vec<u8>,
}

impl OwnedEffect {
    #[must_use]
    pub const fn simple(kind: u32, detail: u32, terminal_id: ResourceId) -> Self {
        Self {
            kind,
            detail,
            status_code: 0,
            terminal_id,
            stream_id: 0,
            bootstrap_id: 0,
            seq: 0,
            first_row: 0,
            last_row: 0,
            bytes: Vec::new(),
        }
    }
}

#[must_use]
pub const fn bytes_out(data: &[u8]) -> PhuxBytes {
    PhuxBytes {
        data: if data.is_empty() {
            ptr::null()
        } else {
            data.as_ptr()
        },
        len: data.len(),
    }
}

#[must_use]
pub fn terminal_id_out(value: &ResourceId) -> PhuxResourceId {
    match value {
        ResourceId::Local { id } => PhuxResourceId {
            kind: 0,
            id: *id,
            host: PhuxBytes::default(),
        },
        ResourceId::Satellite { host, id } => PhuxResourceId {
            kind: 1,
            id: *id,
            host: bytes_out(host.as_str().as_bytes()),
        },
    }
}
