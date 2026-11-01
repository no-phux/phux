//! The helper each pane runs. It keeps the pane's terminal in editor modes
//! and relays bytes: input and resizes to the daemon, frames back. When the
//! daemon hangs up it restores the terminal and exits, and phux closes the
//! pane; when the pane goes away the daemon sees the socket close.
//!
//! Frames to the daemon are a kind byte, a little-endian u32 length and the
//! payload. Bytes from the daemon are raw terminal output.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use signal_hook::consts::SIGWINCH;
use signal_hook::iterator::Signals;

pub const HELLO: u8 = b'h';
pub const INPUT: u8 = b'i';
pub const RESIZE: u8 = b'r';

/// Alternate screen, bracketed paste, focus reports, mouse (1002 drags, 1006
/// SGR encoding), then the kitty keyboard's disambiguate flag.
const EDITOR_ON: &[u8] = b"\x1b[?1049h\x1b[?2004h\x1b[?1004h\x1b[?1002h\x1b[?1006h\x1b[>1u";
/// Undoes `EDITOR_ON`, and resets colors and the cursor.
const EDITOR_OFF: &[u8] =
    b"\x1b[<u\x1b[?1006l\x1b[?1002l\x1b[?1004l\x1b[?2004l\x1b[0m\x1b[0 q\x1b[?25h\x1b[?1049l";

pub fn write_frame(w: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let mut f = Vec::with_capacity(payload.len() + 5);
    f.push(kind);
    f.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    f.extend_from_slice(payload);
    w.write_all(&f)
}

pub fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]);
    let mut body = vec![0; len as usize];
    r.read_exact(&mut body)?;
    Ok((head[0], body))
}

/// `PANE TERMINAL COLS ROWS`, in decimal.
pub fn parse_hello(body: &[u8]) -> Option<[u32; 4]> {
    let text = std::str::from_utf8(body).ok()?;
    let mut f = text.split(' ').map(|n| n.parse().ok());
    Some([f.next()??, f.next()??, f.next()??, f.next()??])
}

/// Cells across and down, from `ioctl(TIOCGWINSZ)`.
fn tty_size(fd: i32) -> (u16, u16) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `ws` is a valid out-pointer for this request.
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &raw mut ws) } != 0 {
        return (80, 24);
    }
    (ws.ws_col.max(1), ws.ws_row.max(1))
}

/// Raw mode for as long as it lives.
struct Raw {
    fd: i32,
    saved: libc::termios,
}

impl Raw {
    fn enter(fd: i32) -> io::Result<Self> {
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `t` is a valid out-pointer and `fd` is open.
        if unsafe { libc::tcgetattr(fd, &raw mut t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let saved = t;
        // SAFETY: `t` was filled by tcgetattr.
        unsafe { libc::cfmakeraw(&raw mut t) };
        // SAFETY: `t` is valid and `fd` is open.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, saved })
    }

    fn restore(&self) {
        let _ = io::stdout().write_all(EDITOR_OFF);
        let _ = io::stdout().flush();
        // SAFETY: `saved` came from tcgetattr on this fd.
        unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &raw const self.saved) };
    }
}

fn connect(socket: &Path) -> io::Result<UnixStream> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(socket) {
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            other => return other,
        }
    }
}

/// Relays this pane's terminal until either side goes away.
pub fn run(socket: &Path, pane: u32) -> io::Result<()> {
    let stdin = io::stdin();
    let fd = stdin.as_raw_fd();
    // SAFETY: isatty only inspects the fd.
    if unsafe { libc::isatty(fd) } != 1 {
        return Err(io::Error::other("phux-nvim needs a terminal"));
    }
    let terminal = std::env::var("PHUX_TERMINAL_ID")
        .ok()
        .and_then(|t| t.parse::<u32>().ok())
        .ok_or_else(|| io::Error::other("phux-nvim runs inside a phux pane"))?;

    // before the hello, so a resize in between isn't missed
    let mut signals = Signals::new([SIGWINCH])?;
    let stream = connect(socket)?;
    let mut from_daemon = stream.try_clone()?;
    let to_daemon = Arc::new(Mutex::new(stream));

    let raw = Arc::new(Raw::enter(fd)?);
    let mut out = io::stdout();
    out.write_all(EDITOR_ON)?;
    out.flush()?;

    let (cols, rows) = tty_size(fd);
    let hello = format!("{pane} {terminal} {cols} {rows}");
    write_frame(&mut *lock(&to_daemon), HELLO, hello.as_bytes())?;

    let resize = Arc::clone(&to_daemon);
    std::thread::spawn(move || {
        for _ in signals.forever() {
            let (c, r) = tty_size(fd);
            let body = format!("{c} {r}");
            if write_frame(&mut *lock(&resize), RESIZE, body.as_bytes()).is_err() {
                return;
            }
        }
    });

    let exit = Arc::clone(&raw);
    std::thread::spawn(move || {
        let mut buf = [0; 64 * 1024];
        let mut out = io::stdout();
        while let Ok(n) = from_daemon.read(&mut buf) {
            if n == 0 || out.write_all(&buf[..n]).is_err() || out.flush().is_err() {
                break;
            }
        }
        exit.restore();
        std::process::exit(0);
    });

    let mut buf = [0; 16 * 1024];
    let mut input = stdin.lock();
    while let Ok(n) = input.read(&mut buf) {
        if n == 0 || write_frame(&mut *lock(&to_daemon), INPUT, &buf[..n]).is_err() {
            break;
        }
    }
    raw.restore();
    Ok(())
}

fn lock(m: &Mutex<UnixStream>) -> std::sync::MutexGuard<'_, UnixStream> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, INPUT, b"abc").unwrap();
        write_frame(&mut buf, RESIZE, b"").unwrap();
        let mut r = buf.as_slice();
        assert_eq!(read_frame(&mut r).unwrap(), (INPUT, b"abc".to_vec()));
        assert_eq!(read_frame(&mut r).unwrap(), (RESIZE, Vec::new()));
        assert!(read_frame(&mut r).is_err());
    }

    #[test]
    fn hellos_parse() {
        assert_eq!(parse_hello(b"0 7 80 24"), Some([0, 7, 80, 24]));
        assert_eq!(parse_hello(b"0 7 80"), None);
    }
}
