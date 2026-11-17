//! Private file-backed stdin and nonblocking output sockets: no pipe
//! deadlocks, and closing a capture stops descendant writers at the sink.

use std::fs::File;
use std::io::{ErrorKind, Read, Seek, Write};
use std::os::{fd::OwnedFd, unix::net::UnixStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MAX_OUTPUT: usize = 2 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(3);

pub(super) fn render(argv: &[String], source: &str, width: u16) -> Result<String, String> {
    run(argv, source, width, TIMEOUT).map_err(|error| format!("{error} · showing source"))
}

fn run(argv: &[String], source: &str, width: u16, timeout: Duration) -> Result<String, String> {
    let (mut command, mut streams) = prepare(argv, source, width)?;
    let mut child = command
        .spawn()
        .map_err(|e| format!("Couldn't start viewer: {e}"))?;
    // Command retains its configured handles across spawn. Only the child
    // should retain output writers.
    drop(command);
    let status = supervise(&mut child, &mut streams, timeout);
    // Only the exact process we spawned, and always reap it.
    if status.is_err() {
        let _ = child.kill();
    }
    let reaped = child.wait().map_err(io_error);
    status?;
    let reaped = reaped?;
    if !reaped.success() {
        let detail: String = streams.errors.text()?.chars().take(240).collect();
        return Err(format!("Viewer {reaped}: {}", detail.trim()));
    }
    streams.output.text()
}

fn prepare(argv: &[String], source: &str, width: u16) -> Result<(Command, Streams), String> {
    let input = input_file(source)?;
    let (output, out_writer) = Capture::pair()?;
    let (errors, err_writer) = Capture::pair()?;
    let mut command = command(argv, width)?;
    command
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(OwnedFd::from(out_writer)))
        .stderr(Stdio::from(OwnedFd::from(err_writer)));
    Ok((command, Streams { output, errors }))
}

fn input_file(source: &str) -> Result<File, String> {
    let mut input = tempfile::tempfile().map_err(io_error)?;
    input.write_all(source.as_bytes()).map_err(io_error)?;
    input.rewind().map_err(io_error)?;
    Ok(input)
}

fn command(argv: &[String], width: u16) -> Result<Command, String> {
    let (executable, args) = argv
        .split_first()
        .filter(|(exe, _)| !exe.is_empty())
        .ok_or("Viewer command is empty")?;
    let width = width.to_string();
    let mut command = Command::new(executable);
    command
        .args(args.iter().map(|arg| arg.replace("{width}", &width)))
        .env("COLUMNS", &width);
    // A renderer has no need for the enclosing agent's server credentials.
    for (key, _) in
        std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("PHUX_"))
    {
        command.env_remove(key);
    }
    Ok(command)
}

struct Streams {
    output: Capture,
    errors: Capture,
}

impl Streams {
    fn drain(&mut self) -> Result<(), String> {
        self.output.drain()?;
        self.errors.drain()
    }
}

struct Capture {
    reader: UnixStream,
    bytes: Vec<u8>,
}

impl Capture {
    fn pair() -> Result<(Self, UnixStream), String> {
        let (reader, writer) = UnixStream::pair().map_err(io_error)?;
        reader.set_nonblocking(true).map_err(io_error)?;
        Ok((
            Self {
                reader,
                bytes: Vec::new(),
            },
            writer,
        ))
    }

    fn drain(&mut self) -> Result<(), String> {
        let mut buffer = [0; 8192];
        loop {
            match self.reader.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    if self.bytes.len() + n > MAX_OUTPUT {
                        return Err("Viewer output exceeded 2 MiB".into());
                    }
                    self.bytes.extend_from_slice(&buffer[..n]);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(io_error(error)),
            }
        }
    }

    fn text(self) -> Result<String, String> {
        let text = String::from_utf8(self.bytes).map_err(|_| "Viewer output is not UTF-8")?;
        Ok(text
            .chars()
            .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
            .collect())
    }
}

fn supervise(child: &mut Child, streams: &mut Streams, timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    loop {
        streams.drain()?;
        if child.try_wait().map_err(io_error)?.is_some() {
            // Include the child's final bytes, but never wait for descendants.
            return streams.drain();
        }
        if started.elapsed() >= timeout {
            return Err("Viewer timed out after its deadline".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn io_error(error: std::io::Error) -> String {
    format!("Viewer I/O failed: {error}")
}

#[cfg(test)]
pub(super) fn test_run(
    argv: &[String],
    source: &str,
    width: u16,
    timeout: Duration,
) -> Result<String, String> {
    run(argv, source, width, timeout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_a_full_capture_stops_an_independent_writer() {
        let (mut capture, mut writer) = Capture::pair().unwrap();
        let writing = std::thread::spawn(move || while writer.write_all(&[b'x'; 8192]).is_ok() {});
        let started = Instant::now();
        while capture.drain().is_ok() {
            assert!(started.elapsed() < Duration::from_secs(3));
            std::thread::yield_now();
        }
        assert!(capture.bytes.len() <= MAX_OUTPUT);
        drop(capture);
        writing.join().unwrap();
    }
}
