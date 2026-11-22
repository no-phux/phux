//! Native input for the Linux surface mirror: the wlroots virtual-pointer
//! (`zwlr_virtual_pointer_v1`) and virtual-keyboard
//! (`zwp_virtual_keyboard_v1`) protocols, spoken straight over the
//! compositor's Wayland socket. A dozen requests need no bindings: a message
//! is little-endian words, and the one file descriptor (the keymap) rides
//! `SCM_RIGHTS`. Hyprland and Sway offer both; without them the mirror falls
//! back to the command-line tools (`surface_grim_input`).
//!
//! [`ops`] turns an intent into [`Op`]s, pure and tested everywhere;
//! `Session` (Linux only) sends them on a fresh connection per intent.
//!
//! The pointer moves with `motion_absolute` on a device bound to no output,
//! which the compositor reads as a fraction of the whole layout, the union
//! of every output in logical pixels. Text types through a keymap made for
//! it, one keycode per distinct character, as `wtype` does.

#![cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "macOS builds this module only to test it")
)]

use std::fmt::Write as _;

use phux_client::tree::DocumentInput;

use super::surface_grim::Rect;
use super::surface_grim_input::{Place, keysym, to_layout};

/// The global that makes virtual pointers.
pub(super) const POINTER: &str = "zwlr_virtual_pointer_manager_v1";
/// The global that makes virtual keyboards.
pub(super) const KEYBOARD: &str = "zwp_virtual_keyboard_manager_v1";
/// Distinct keysyms one keymap carries; xkb keycodes stop at 255.
const KEYMAP_KEYS: usize = 200;

/// One step of an intent on the virtual devices.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Op {
    /// Put the pointer at `(x, y)` of the `w`x`h` layout box.
    Move { x: u32, y: u32, w: u32, h: u32 },
    /// Press (true) or release (false) the left button.
    Button(bool),
    /// Scroll by layout pixels, down and right positive.
    Scroll { dx: f64, dy: f64 },
    /// Load `keymap`, press the `held` key codes (modifier keys) and
    /// hold the modifiers in mask `mods` (none when 0), then press and
    /// release each key code in order.
    Keys {
        keymap: String,
        codes: Vec<u32>,
        mods: u32,
        held: Vec<u32>,
    },
}

#[path = "surface_wayland_chord.rs"]
mod chord;
pub(super) use chord::chord;

/// The steps that carry out `input` on an `image`-sized frame placed at
/// `place`, or why they cannot.
pub(super) fn ops(
    input: &DocumentInput,
    image: (u32, u32),
    place: Option<&Place>,
) -> Result<Vec<Op>, String> {
    let need =
        || place.ok_or("the pointer needs Hyprland (hyprctl) to place it; run under Hyprland");
    Ok(match input {
        // Chords are [`chord`]; the clipboard is the mirror's.
        DocumentInput::Navigate { .. }
        | DocumentInput::Paste { .. }
        | DocumentInput::SelectAll
        | DocumentInput::CopySelection { .. }
        | DocumentInput::CopyClipboard { .. }
        | DocumentInput::ClipboardText { .. }
        | DocumentInput::Activate { .. } => Vec::new(),
        DocumentInput::Click { x, y } => vec![
            at((*x, *y), image, need()?)?,
            Op::Button(true),
            Op::Button(false),
        ],
        DocumentInput::Drag { from, to } => {
            let place = need()?;
            let path = super::surface_input::drag_path(*from, *to);
            let mut out = vec![at(path[0], image, place)?, Op::Button(true)];
            for point in &path[1..] {
                out.push(at(*point, image, place)?);
            }
            out.push(Op::Button(false));
            out
        }
        DocumentInput::Scroll { dx, dy } => {
            let place = need()?;
            let ratio = place.frame.w / f64::from(image.0.max(1));
            let mut out = Vec::new();
            // A window scrolls under the pointer, so put it there.
            if place.window.is_some() {
                out.push(at((1, 1), (2, 2), place)?);
            }
            out.push(Op::Scroll {
                dx: f64::from(*dx) * ratio,
                dy: f64::from(*dy) * ratio,
            });
            out
        }
        DocumentInput::Type { text } => typed(text),
        DocumentInput::Key { key } => vec![keys(&[keysym(*key).to_owned()], vec![1])],
    })
}

/// A move to image pixel `point`, relative to the layout's corner.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "layout pixels are small and clamped non-negative"
)]
fn at(point: (u32, u32), image: (u32, u32), place: &Place) -> Result<Op, String> {
    let (x, y) = to_layout(point, image, place.frame)?;
    let Rect { x: lx, y: ly, w, h } = place.layout;
    let rel = |v: i64, origin: f64| (v - origin.floor() as i64).max(0) as u32;
    Ok(Op::Move {
        x: rel(x, lx),
        y: rel(y, ly),
        w: w.ceil().max(1.0) as u32,
        h: h.ceil().max(1.0) as u32,
    })
}

/// `text` as key presses, a keymap per [`KEYMAP_KEYS`] distinct characters.
/// Control characters other than newline and tab have no key and are
/// dropped.
fn typed(text: &str) -> Vec<Op> {
    let mut out = Vec::new();
    let (mut syms, mut codes) = (Vec::<String>::new(), Vec::new());
    for c in text.chars() {
        let sym = match c {
            '\n' | '\r' => "Return".to_owned(),
            '\t' => "Tab".to_owned(),
            c if c.is_control() => continue,
            c => format!("U{:04X}", u32::from(c)),
        };
        let index = match syms.iter().position(|s| *s == sym) {
            Some(index) => index,
            None if syms.len() == KEYMAP_KEYS => {
                out.push(keys(&std::mem::take(&mut syms), std::mem::take(&mut codes)));
                syms.push(sym);
                0
            }
            None => {
                syms.push(sym);
                syms.len() - 1
            }
        };
        codes.push(u32::try_from(index).unwrap_or(0) + 1);
    }
    if !codes.is_empty() {
        out.push(keys(&syms, codes));
    }
    out
}

/// A keymap where key code `i + 1` (xkb keycode `i + 9`) types `syms[i]`.
fn keys(syms: &[String], codes: Vec<u32>) -> Op {
    let mut keymap = format!(
        "xkb_keymap {{\nxkb_keycodes \"phux\" {{\nminimum = 8;\nmaximum = {};\n",
        syms.len() + 9
    );
    for i in 1..=syms.len() {
        let _ = writeln!(keymap, "<K{i}> = {};", i + 8);
    }
    keymap.push_str(
        "};\nxkb_types \"phux\" { include \"complete\" };\n\
         xkb_compatibility \"phux\" { include \"complete\" };\nxkb_symbols \"phux\" {\n",
    );
    for (i, sym) in syms.iter().enumerate() {
        let _ = writeln!(keymap, "key <K{}> {{[{sym}]}};", i + 1);
    }
    keymap.push_str("};\n};\n");
    Op::Keys {
        keymap,
        codes,
        mods: 0,
        held: Vec::new(),
    }
}

/// One argument of a request.
enum Arg<'a> {
    U(u32),
    /// A 24.8 fixed-point number.
    Fixed(f64),
    S(&'a str),
}

/// The bytes of request `opcode` on `object`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "fixed-point scroll deltas are small"
)]
fn message(object: u32, opcode: u16, args: &[Arg<'_>]) -> Vec<u8> {
    let mut out = object.to_le_bytes().to_vec();
    out.extend_from_slice(&[0; 4]);
    for arg in args {
        match arg {
            Arg::U(v) => out.extend_from_slice(&v.to_le_bytes()),
            Arg::Fixed(v) => out.extend_from_slice(&((v * 256.0).round() as i32).to_le_bytes()),
            Arg::S(s) => {
                let len = u32::try_from(s.len() + 1).unwrap_or(u32::MAX);
                out.extend_from_slice(&len.to_le_bytes());
                out.extend_from_slice(s.as_bytes());
                out.push(0);
                out.resize(out.len().next_multiple_of(4), 0);
            }
        }
    }
    let size = u32::try_from(out.len()).unwrap_or(0) << 16 | u32::from(opcode);
    out[4..8].copy_from_slice(&size.to_le_bytes());
    out
}

#[cfg(target_os = "linux")]
pub(super) use wire::Session;

#[cfg(target_os = "linux")]
mod wire {
    use std::io::{IoSlice, Read as _, Write as _};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use super::{Arg, KEYBOARD, Op, POINTER, message};

    const DISPLAY: u32 = 1;
    const REGISTRY: u32 = 2;
    const BTN_LEFT: u32 = 0x110;

    /// One connection to the compositor and the globals it offers.
    pub(in super::super) struct Session {
        stream: UnixStream,
        next: u32,
        globals: Vec<(u32, String, u32)>,
        seat: Option<u32>,
        pointer: Option<u32>,
        keyboard: Option<u32>,
    }

    impl Session {
        /// Connect to the session's compositor and list its globals.
        pub(in super::super) fn connect(env: &[(&str, String)]) -> Result<Self, String> {
            let get = |key: &str| env.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str());
            let display = get("WAYLAND_DISPLAY").ok_or("no Wayland display in this session")?;
            let path = Path::new(get("XDG_RUNTIME_DIR").unwrap_or("/")).join(display);
            let stream = UnixStream::connect(&path).map_err(|err| {
                format!("the compositor at {} did not answer: {err}", path.display())
            })?;
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .map_err(|err| err.to_string())?;
            let mut session = Self {
                stream,
                next: REGISTRY + 1,
                globals: Vec::new(),
                seat: None,
                pointer: None,
                keyboard: None,
            };
            session.put(&message(DISPLAY, 1, &[Arg::U(REGISTRY)]))?;
            session.roundtrip()?;
            Ok(session)
        }

        /// Whether the compositor offers global `interface`.
        pub(in super::super) fn has(&self, interface: &str) -> bool {
            self.globals.iter().any(|(_, name, _)| name == interface)
        }

        /// Carry out `ops`, then wait until the compositor has read them.
        pub(in super::super) fn run(mut self, ops: &[Op]) -> Result<(), String> {
            for op in ops {
                match op {
                    Op::Move { x, y, w, h } => {
                        let p = self.pointer()?;
                        let at = [
                            Arg::U(now()),
                            Arg::U(*x),
                            Arg::U(*y),
                            Arg::U(*w),
                            Arg::U(*h),
                        ];
                        self.put(&message(p, 1, &at))?;
                        self.put(&message(p, 4, &[]))?;
                        // A drag's steps must read as movement over time.
                        std::thread::sleep(Duration::from_millis(8));
                    }
                    Op::Button(pressed) => {
                        let p = self.pointer()?;
                        let state = u32::from(*pressed);
                        self.put(&message(
                            p,
                            2,
                            &[Arg::U(now()), Arg::U(BTN_LEFT), Arg::U(state)],
                        ))?;
                        self.put(&message(p, 4, &[]))?;
                    }
                    Op::Scroll { dx, dy } => {
                        let p = self.pointer()?;
                        for (axis, d) in [(0, *dy), (1, *dx)] {
                            if d != 0.0 {
                                self.put(&message(
                                    p,
                                    3,
                                    &[Arg::U(now()), Arg::U(axis), Arg::Fixed(d)],
                                ))?;
                            }
                        }
                        self.put(&message(p, 4, &[]))?;
                    }
                    Op::Keys {
                        keymap,
                        codes,
                        mods,
                        held,
                    } => {
                        let k = self.keyboard()?;
                        self.keymap(k, keymap)?;
                        let key = |code: u32, state: u32| {
                            message(k, 1, &[Arg::U(now()), Arg::U(code), Arg::U(state)])
                        };
                        // modifiers(depressed, latched, locked, group)
                        let hold = |m| message(k, 2, &[Arg::U(m), Arg::U(0), Arg::U(0), Arg::U(0)]);
                        // The modifier keys make this keyboard the active one
                        // first, so the mask lands after its keymap.
                        for code in held {
                            self.put(&key(*code, 1))?;
                        }
                        if *mods != 0 {
                            self.put(&hold(*mods))?;
                            self.roundtrip()?;
                        }
                        for code in codes {
                            for state in [1, 0] {
                                self.put(&key(*code, state))?;
                            }
                        }
                        if *mods != 0 {
                            self.put(&hold(0))?;
                        }
                        for code in held.iter().rev() {
                            self.put(&key(*code, 0))?;
                        }
                    }
                }
            }
            self.roundtrip()
        }

        const fn new_id(&mut self) -> u32 {
            self.next += 1;
            self.next - 1
        }

        /// Bind global `interface` at version 1.
        fn bind(&mut self, interface: &str) -> Result<u32, String> {
            let name = self
                .globals
                .iter()
                .find(|(_, n, _)| n == interface)
                .map(|(name, _, _)| *name)
                .ok_or_else(|| format!("the compositor offers no {interface}"))?;
            let id = self.new_id();
            self.put(&message(
                REGISTRY,
                0,
                &[Arg::U(name), Arg::S(interface), Arg::U(1), Arg::U(id)],
            ))?;
            Ok(id)
        }

        fn seat(&mut self) -> Result<u32, String> {
            if let Some(seat) = self.seat {
                return Ok(seat);
            }
            let seat = self.bind("wl_seat")?;
            self.seat = Some(seat);
            Ok(seat)
        }

        fn pointer(&mut self) -> Result<u32, String> {
            if let Some(pointer) = self.pointer {
                return Ok(pointer);
            }
            let (seat, manager) = (self.seat()?, self.bind(POINTER)?);
            let pointer = self.new_id();
            self.put(&message(manager, 0, &[Arg::U(seat), Arg::U(pointer)]))?;
            self.pointer = Some(pointer);
            Ok(pointer)
        }

        fn keyboard(&mut self) -> Result<u32, String> {
            if let Some(keyboard) = self.keyboard {
                return Ok(keyboard);
            }
            let (seat, manager) = (self.seat()?, self.bind(KEYBOARD)?);
            let keyboard = self.new_id();
            self.put(&message(manager, 0, &[Arg::U(seat), Arg::U(keyboard)]))?;
            self.keyboard = Some(keyboard);
            Ok(keyboard)
        }

        /// Hand `keyboard` the xkb `keymap` in a memfd, and wait until it
        /// is loaded.
        fn keymap(&mut self, keyboard: u32, keymap: &str) -> Result<(), String> {
            use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
            let err =
                |err: std::io::Error| format!("the keymap did not reach the compositor: {err}");
            let fd = rustix::fs::memfd_create("phux-keymap", rustix::fs::MemfdFlags::CLOEXEC)
                .map_err(|e| err(e.into()))?;
            let mut file = std::fs::File::from(fd);
            file.write_all(keymap.as_bytes())
                .and_then(|()| file.write_all(&[0]))
                .map_err(err)?;
            let size = u32::try_from(keymap.len() + 1).unwrap_or(u32::MAX);
            // Format 1 is `xkb_v1`; the fd travels beside the words.
            let bytes = message(keyboard, 0, &[Arg::U(1), Arg::U(size)]);
            let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
            let mut control = SendAncillaryBuffer::new(&mut space);
            let fds = [std::os::fd::AsFd::as_fd(&file)];
            control.push(SendAncillaryMessage::ScmRights(&fds));
            let sent = sendmsg(
                &self.stream,
                &[IoSlice::new(&bytes)],
                &mut control,
                SendFlags::empty(),
            )
            .map_err(|e| err(e.into()))?;
            if sent != bytes.len() {
                return Err(err(std::io::ErrorKind::WriteZero.into()));
            }
            self.roundtrip()
        }

        fn put(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.stream
                .write_all(bytes)
                .map_err(|err| format!("the compositor connection failed: {err}"))
        }

        /// `wl_display.sync`, reading events (globals, an error) until done.
        fn roundtrip(&mut self) -> Result<(), String> {
            let callback = self.new_id();
            self.put(&message(DISPLAY, 0, &[Arg::U(callback)]))?;
            let lost = |err: std::io::Error| format!("the compositor did not answer: {err}");
            loop {
                let mut header = [0; 8];
                self.stream.read_exact(&mut header).map_err(lost)?;
                let word = |i: usize, b: &[u8]| {
                    u32::from_le_bytes(
                        b.get(i..i + 4)
                            .and_then(|s| s.try_into().ok())
                            .unwrap_or([0; 4]),
                    )
                };
                let (object, size_op) = (word(0, &header), word(4, &header));
                let mut body = vec![0; ((size_op >> 16) as usize).saturating_sub(8)];
                self.stream.read_exact(&mut body).map_err(lost)?;
                let string = |at: usize| {
                    let len = word(at, &body) as usize;
                    let bytes = body
                        .get(at + 4..at + 4 + len.saturating_sub(1))
                        .unwrap_or_default();
                    (
                        String::from_utf8_lossy(bytes).into_owned(),
                        at + 4 + len.next_multiple_of(4),
                    )
                };
                match (object, size_op & 0xffff) {
                    (DISPLAY, 0) => {
                        let (text, _) = string(8);
                        return Err(format!("the compositor refused the input: {text}"));
                    }
                    (REGISTRY, 0) => {
                        let (interface, end) = string(4);
                        self.globals
                            .push((word(0, &body), interface, word(end, &body)));
                    }
                    (id, 0) if id == callback => return Ok(()),
                    _ => {}
                }
            }
        }
    }

    /// Milliseconds on this process's clock, the events' timestamps.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the timestamp wraps, as Wayland's does"
    )]
    fn now() -> u32 {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_millis() as u32
    }
}

#[cfg(test)]
#[path = "surface_wayland_tests.rs"]
mod tests;
