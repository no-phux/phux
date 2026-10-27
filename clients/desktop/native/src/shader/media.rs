//! Video for shaders: the frames of a file or URL that a `// phux:media`
//! line names, decoded by `ffmpeg` and sampled with `phux_media(uv)`.
//!
//! One [`Feed`] per source is shared by every renderer that shows it and
//! stops its decoder when the last one lets go. A file loops; a URL is
//! resolved through `yt-dlp` when it is installed, so a YouTube or Twitch
//! link plays, and otherwise handed to `ffmpeg` as is. A decoder that exits
//! starts again, after 1, 5, then 30 seconds when it keeps failing.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

/// Longest side of a decoded frame, in pixels; the backdrop renders at most
/// 1024, and every GPU takes a texture this size.
pub const MAX_SIDE: u32 = 960;
/// Frames a second decoded: the backdrop's own top rate.
const FPS: u32 = 30;
const RETRY: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
];
/// A decoder that ran this long was working; its exit starts the backoff over.
const HEALTHY: Duration = Duration::from_secs(10);

/// One decoded frame: tight RGBA rows.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Counts frames, so a renderer uploads each one once.
    pub serial: u64,
}

struct Shared {
    source: String,
    latest: Mutex<Option<Arc<Picture>>>,
    stop: AtomicBool,
    child: Mutex<Option<Child>>,
}

/// A running decoder. Dropping the last handle stops it.
pub struct Feed {
    shared: Arc<Shared>,
}

static FEEDS: Mutex<Option<HashMap<String, Weak<Feed>>>> = Mutex::new(None);

/// The feed for `source`, starting its decoder if no one shows it yet.
#[must_use]
pub fn feed(source: &str) -> Arc<Feed> {
    let mut feeds = FEEDS.lock().unwrap_or_else(PoisonError::into_inner);
    let feeds = feeds.get_or_insert_with(HashMap::new);
    feeds.retain(|_, feed| feed.strong_count() > 0);
    if let Some(feed) = feeds.get(source).and_then(Weak::upgrade) {
        return feed;
    }
    let shared = Arc::new(Shared {
        source: source.to_owned(),
        latest: Mutex::new(None),
        stop: AtomicBool::new(false),
        child: Mutex::new(None),
    });
    let decoder = shared.clone();
    let spawned = std::thread::Builder::new()
        .name("phux-shader-media".into())
        .spawn(move || decode(&decoder));
    if let Err(error) = spawned {
        log::warn!("shader media {source}: could not start its decoder: {error}");
    }
    let feed = Arc::new(Feed { shared });
    feeds.insert(source.to_owned(), Arc::downgrade(&feed));
    feed
}

impl Feed {
    /// The newest frame, once one has decoded.
    #[must_use]
    pub fn latest(&self) -> Option<Arc<Picture>> {
        self.shared
            .latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.shared.source
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        // Killing the decoder ends the read the thread is blocked in.
        if let Some(child) = self
            .shared
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            let _ = child.kill();
        }
    }
}

fn decode(shared: &Shared) {
    let mut failures = 0usize;
    let mut serial = 0u64;
    let mut reported = None;
    while !shared.stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let outcome = run(shared, &mut serial);
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
        if started.elapsed() >= HEALTHY {
            failures = 0;
        }
        if let Err(error) = &outcome
            && reported.as_ref() != Some(error)
        {
            log::warn!("shader media {}: {error}", shared.source);
            reported = Some(error.clone());
        }
        let pause = RETRY[failures.min(RETRY.len() - 1)];
        failures += 1;
        let until = Instant::now() + pause;
        while Instant::now() < until && !shared.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Decode until the stream ends, the decoder fails, or the feed stops.
fn run(shared: &Shared, serial: &mut u64) -> Result<(), String> {
    let input = resolve(&shared.source)?;
    let mut child = decoder(&input)?
        .spawn()
        .map_err(|error| format!("could not run ffmpeg: {error}"))?;
    let stdout = child.stdout.take().ok_or("ffmpeg has no output")?;
    // Drained on its own thread so a chatty decoder never blocks on it.
    let stderr = child
        .stderr
        .take()
        .map(|stderr| std::thread::spawn(move || last_line(stderr)));
    if !adopt(shared, child) {
        return Ok(());
    }
    let pumped = pump(shared, stdout, serial);
    let child = shared
        .child
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    let status = child.and_then(|mut child| {
        let _ = child.kill();
        child.wait().ok()
    });
    let said = stderr
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    match (pumped?, status) {
        (0, Some(status)) if !status.success() => Err(if said.is_empty() {
            format!("ffmpeg exited with {status}")
        } else {
            said
        }),
        _ => Ok(()),
    }
}

/// `ffmpeg` writing `input` to stdout as PPM frames, a file looping.
fn decoder(input: &str) -> Result<Command, String> {
    let ffmpeg = tool("ffmpeg").ok_or("install ffmpeg to play shader media")?;
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-re"]);
    if !input.starts_with("http://") && !input.starts_with("https://") {
        command.args(["-stream_loop", "-1"]);
    }
    let filter = format!(
        "fps={FPS},scale='min({MAX_SIDE},iw)':'min({MAX_SIDE},ih)'\
         :force_original_aspect_ratio=decrease:force_divisible_by=2"
    );
    command
        .args(["-i", input, "-an", "-sn", "-vf", &filter])
        .args(["-f", "image2pipe", "-vcodec", "ppm", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Ok(command)
}

/// Hand `child` to the feed so dropping it can kill the decoder; a feed
/// already stopped kills it here instead.
fn adopt(shared: &Shared, mut child: Child) -> bool {
    let mut slot = shared.child.lock().unwrap_or_else(PoisonError::into_inner);
    if shared.stop.load(Ordering::Relaxed) {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    }
    *slot = Some(child);
    true
}

/// Publish every frame the decoder writes: how many, or why one was
/// malformed.
fn pump(shared: &Shared, stdout: impl Read, serial: &mut u64) -> Result<u64, String> {
    let mut reader = BufReader::with_capacity(1 << 20, stdout);
    let mut decoded = 0;
    while let Some((width, height, rgba)) = read_ppm(&mut reader)? {
        *serial += 1;
        decoded += 1;
        let picture = Arc::new(Picture {
            width,
            height,
            rgba,
            serial: *serial,
        });
        *shared.latest.lock().unwrap_or_else(PoisonError::into_inner) = Some(picture);
    }
    Ok(decoded)
}

/// The last line a decoder wrote to stderr, read to its end.
fn last_line(stderr: impl Read) -> String {
    let mut last = String::new();
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { break };
        if !line.trim().is_empty() {
            last = line.trim().chars().take(300).collect();
        }
    }
    last
}

/// What `ffmpeg` reads for `source`: a URL as `yt-dlp` resolves it, or a
/// path with `~` expanded and relative to the shader folder.
fn resolve(source: &str) -> Result<String, String> {
    if source.starts_with("http://") || source.starts_with("https://") {
        return Ok(stream_url(source).unwrap_or_else(|| source.to_owned()));
    }
    let path = expand(source, crate::platform::home_dir().as_deref());
    let path = if path.is_absolute() {
        path
    } else {
        super::library::folder().join(path)
    };
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    Ok(path.to_string_lossy().into_owned())
}

/// `~` and `~/...` under `home`.
fn expand(source: &str, home: Option<&Path>) -> PathBuf {
    match (source.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home.to_path_buf(),
        (Some(rest), Some(home)) if rest.starts_with('/') => home.join(&rest[1..]),
        _ => PathBuf::from(source),
    }
}

/// A page's direct video stream, when `yt-dlp` is installed and knows it.
fn stream_url(page: &str) -> Option<String> {
    let ytdlp = tool("yt-dlp")?;
    let output = Command::new(ytdlp)
        .args([
            "--no-playlist",
            "--quiet",
            "--no-warnings",
            "-f",
            "bv*[height<=720][vcodec!=none]/b[height<=720]/bv*/b",
            "-g",
            page,
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("http"))
        .map(str::to_owned)
}

/// A command on `PATH` or where package managers put it: an app opened
/// from the Dock does not inherit the shell's `PATH`.
pub(crate) fn tool(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    dirs.extend(
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]
            .into_iter()
            .map(PathBuf::from),
    );
    if let Some(home) = crate::platform::home_dir() {
        dirs.push(home.join(".local/bin"));
    }
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    dirs.into_iter()
        .map(|dir| dir.join(&file))
        .find(|path| path.is_file())
}

/// One binary PPM (`P6`, 8-bit) frame as RGBA, or `None` at a clean end.
fn read_ppm(reader: &mut impl BufRead) -> Result<Option<(u32, u32, Vec<u8>)>, String> {
    if reader
        .fill_buf()
        .map_err(|error| error.to_string())?
        .is_empty()
    {
        return Ok(None);
    }
    let (width, height) = ppm_header(reader)?;
    let mut rgb = vec![0; width as usize * height as usize * 3];
    reader
        .read_exact(&mut rgb)
        .map_err(|error| format!("a frame ended early: {error}"))?;
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for pixel in rgb.as_chunks::<3>().0 {
        rgba.extend_from_slice(pixel);
        rgba.push(255);
    }
    Ok(Some((width, height, rgba)))
}

/// A `P6` header's width and height, for 8-bit frames of a sane size.
fn ppm_header(reader: &mut impl BufRead) -> Result<(u32, u32), String> {
    let magic = token(reader)?;
    if magic != "P6" {
        return Err(format!("ffmpeg sent `{magic}`, not a PPM frame"));
    }
    let mut header = [0u32; 3];
    for value in &mut header {
        *value = token(reader)?
            .parse()
            .map_err(|_| "a PPM header is not numeric".to_owned())?;
    }
    let [width, height, max] = header;
    let sane = (1..=8192).contains(&width) && (1..=8192).contains(&height);
    if max != 255 || !sane {
        return Err(format!(
            "a {width}x{height} frame of depth {max} is not supported"
        ));
    }
    Ok((width, height))
}

/// The next header token, consuming the one whitespace byte after it.
fn token(reader: &mut impl BufRead) -> Result<String, String> {
    let mut text = String::new();
    let mut byte = [0u8];
    loop {
        reader
            .read_exact(&mut byte)
            .map_err(|error| format!("a PPM header ended early: {error}"))?;
        if byte[0].is_ascii_whitespace() {
            if text.is_empty() {
                continue;
            }
            return Ok(text);
        }
        if text.len() >= 16 {
            return Err("a PPM header token is too long".into());
        }
        text.push(char::from(byte[0]));
    }
}

#[cfg(test)]
mod tests;
