use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

use bytes::BytesMut;

use super::{Gathered, MasterFile, PTY_READ_BATCH, PTY_READ_CHUNK, PtySource, gather_wake};

fn pattern_byte(index: usize) -> u8 {
    u8::try_from(index % 251).expect("modulo 251 fits in a byte")
}

/// Run `work` on another thread so a blocked read fails the test instead
/// of hanging the suite.
fn within<T: Send + 'static>(budget: Duration, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(budget).unwrap_or_else(|_| {
        panic!("read was not handed off within {budget:?}");
    })
}

enum Step {
    Data(Vec<u8>),
    Eof,
    Interrupted,
    Error(std::io::ErrorKind),
}

struct Script {
    steps: VecDeque<Step>,
    pulls: usize,
}

impl PtySource for Script {
    fn pull(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
        self.pulls += 1;
        match self.steps.pop_front() {
            Some(Step::Data(data)) => {
                let n = data.len().min(dst.len());
                dst[..n].copy_from_slice(&data[..n]);
                if n < data.len() {
                    self.steps.push_front(Step::Data(data[n..].to_vec()));
                }
                Ok(n)
            }
            Some(Step::Eof) => Ok(0),
            Some(Step::Interrupted) => Err(std::io::Error::from(std::io::ErrorKind::Interrupted)),
            Some(Step::Error(kind)) => Err(std::io::Error::new(kind, "scripted read")),
            None => Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
        }
    }
}

struct Pattern {
    next: usize,
    end: usize,
    chunk: usize,
}

/// A writer that stays open: once drained, reads would block.
impl PtySource for Pattern {
    fn pull(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
        if self.next >= self.end {
            return Err(std::io::Error::from(std::io::ErrorKind::WouldBlock));
        }
        let n = dst.len().min(self.chunk).min(self.end - self.next);
        for (offset, byte) in dst[..n].iter_mut().enumerate() {
            *byte = pattern_byte(self.next + offset);
        }
        self.next += n;
        Ok(n)
    }
}

fn gather(source: &mut impl PtySource, batch: &mut BytesMut) -> Gathered {
    let mut scratch = vec![0_u8; PTY_READ_CHUNK];
    gather_wake(source, &mut scratch, batch)
}

#[test]
fn short_chunks_keep_their_order_across_a_would_block() {
    let mut script = Script {
        steps: VecDeque::from([
            Step::Data(b"one".to_vec()),
            Step::Data(b"-two".to_vec()),
            Step::Data(b"-three".to_vec()),
        ]),
        pulls: 0,
    };
    let mut batch = BytesMut::with_capacity(PTY_READ_BATCH);
    let gathered = gather(&mut script, &mut batch);
    assert_eq!(gathered, Gathered::Bytes { eof: false });
    assert_eq!(&batch[..], b"one-two-three");
    assert_eq!(
        script.pulls, 4,
        "the wake must stop at the first read that would block"
    );

    script.steps.push_back(Step::Data(b"-four".to_vec()));
    let gathered = gather(&mut script, &mut batch);
    assert_eq!(gathered, Gathered::Bytes { eof: false });
    assert_eq!(&batch[..], b"-four");
}

#[test]
fn a_single_small_read_is_handed_off_without_waiting() {
    let (gathered, bytes, pulls, elapsed) = within(Duration::from_secs(1), || {
        let mut script = Script {
            steps: VecDeque::from([Step::Data(b"Z".to_vec())]),
            pulls: 0,
        };
        let mut batch = BytesMut::with_capacity(PTY_READ_BATCH);
        let started = Instant::now();
        let gathered = gather(&mut script, &mut batch);
        (
            gathered,
            Vec::from(&batch[..]),
            script.pulls,
            started.elapsed(),
        )
    });
    assert_eq!(gathered, Gathered::Bytes { eof: false });
    assert_eq!(bytes, b"Z");
    assert_eq!(pulls, 2, "the second read finds nothing and ends the wake");
    assert!(
        elapsed < Duration::from_millis(200),
        "a single byte must not sit on a coalesce timer, waited {elapsed:?}"
    );
}

#[test]
fn the_cap_is_respected_and_the_remainder_stays_in_order() {
    let total = PTY_READ_BATCH * 2 + 31;
    let mut pattern = Pattern {
        next: 0,
        end: total,
        chunk: 10_000,
    };
    let mut batch = BytesMut::new();
    let mut got = Vec::with_capacity(total);
    let mut lengths = Vec::new();
    loop {
        match gather(&mut pattern, &mut batch) {
            Gathered::Bytes { eof: false } => {
                assert!(
                    batch.len() <= PTY_READ_BATCH,
                    "batch of {} exceeds the cap",
                    batch.len()
                );
                assert!(!batch.is_empty());
                lengths.push(batch.len());
                got.extend_from_slice(&batch);
                let _ = batch.split();
            }
            Gathered::Pending => break,
            other => panic!("unexpected {other:?} after {} bytes", got.len()),
        }
    }
    assert_eq!(
        lengths[0], PTY_READ_BATCH,
        "queued data fills the cap exactly"
    );
    assert_eq!(lengths[1], PTY_READ_BATCH);
    assert_eq!(lengths[2], 31);
    assert_eq!(got.len(), total);
    assert!(
        got.iter()
            .enumerate()
            .all(|(index, byte)| *byte == pattern_byte(index)),
        "bytes were lost or reordered"
    );
}

#[test]
fn eof_and_errors_keep_the_bytes_already_read() {
    let mut eof_after = Script {
        steps: VecDeque::from([Step::Data(b"tail".to_vec()), Step::Eof]),
        pulls: 0,
    };
    let mut batch = BytesMut::new();
    assert_eq!(
        gather(&mut eof_after, &mut batch),
        Gathered::Bytes { eof: true }
    );
    assert_eq!(&batch[..], b"tail");

    let mut bare_eof = Script {
        steps: VecDeque::from([Step::Eof]),
        pulls: 0,
    };
    assert_eq!(gather(&mut bare_eof, &mut batch), Gathered::Eof);
    assert!(batch.is_empty());

    let mut broken = Script {
        steps: VecDeque::from([
            Step::Interrupted,
            Step::Data(b"xy".to_vec()),
            Step::Error(std::io::ErrorKind::BrokenPipe),
        ]),
        pulls: 0,
    };
    assert_eq!(
        gather(&mut broken, &mut batch),
        Gathered::Bytes { eof: true }
    );
    assert_eq!(&batch[..], b"xy");
}

/// A pipe whose read end is nonblocking, as the PTY master is.
fn pipe_pair() -> (File, File) {
    let (read, write) = nix::unistd::pipe().expect("pipe");
    let read = File::from(read);
    super::set_nonblock(&read).expect("nonblocking read end");
    (read, File::from(write))
}

#[test]
fn a_single_pipe_byte_is_handed_off_without_blocking_for_more() {
    let (gathered, bytes, elapsed) = within(Duration::from_secs(2), || {
        let (mut read, mut write) = pipe_pair();
        write.write_all(b"Z").expect("write one byte");
        let mut batch = BytesMut::with_capacity(PTY_READ_BATCH);
        let started = Instant::now();
        let gathered = gather(&mut MasterFile { file: &mut read }, &mut batch);
        (gathered, Vec::from(&batch[..]), started.elapsed())
    });
    assert_eq!(gathered, Gathered::Bytes { eof: false });
    assert_eq!(bytes, b"Z");
    assert!(
        elapsed < Duration::from_millis(200),
        "the follow-up read must not wait, waited {elapsed:?}"
    );
}

#[test]
fn closing_the_writer_delivers_the_tail_and_then_eof() {
    let (mut read, mut write) = pipe_pair();
    write.write_all(b"abcdef").expect("write");
    drop(write);
    let mut batch = BytesMut::new();
    let (gathered, bytes) = within(Duration::from_secs(2), move || {
        let gathered = gather(&mut MasterFile { file: &mut read }, &mut batch);
        (gathered, Vec::from(&batch[..]))
    });
    assert_eq!(gathered, Gathered::Bytes { eof: true });
    assert_eq!(bytes, b"abcdef");
}

/// The kernel pipe buffer has to hold more than one batch, which
/// `F_SETPIPE_SZ` provides on Linux.
#[cfg(target_os = "linux")]
#[test]
fn a_prefilled_pipe_stops_at_the_cap_and_continues_in_order() {
    let total = PTY_READ_BATCH * 2 + 31;
    let payload: Vec<u8> = (0..total).map(pattern_byte).collect();
    let (first, lengths, got) = within(Duration::from_secs(2), move || {
        let (read, mut write) = pipe_pair();
        nix::fcntl::fcntl(&read, nix::fcntl::FcntlArg::F_SETPIPE_SZ(256 * 1024))
            .expect("enlarge the pipe");
        write.write_all(&payload).expect("prefill the pipe");
        let mut read = read;
        let mut batch = BytesMut::new();
        let first = gather(&mut MasterFile { file: &mut read }, &mut batch);
        let mut got = Vec::from(&batch[..]);
        let mut lengths = vec![got.len()];
        let _ = batch.split();
        while got.len() < total {
            match gather(&mut MasterFile { file: &mut read }, &mut batch) {
                Gathered::Bytes { eof: false } => {
                    lengths.push(batch.len());
                    got.extend_from_slice(&batch);
                    let _ = batch.split();
                }
                other => panic!("unexpected {other:?} after {} bytes", got.len()),
            }
        }
        (first, lengths, got)
    });
    assert_eq!(first, Gathered::Bytes { eof: false });
    assert_eq!(lengths, vec![PTY_READ_BATCH, PTY_READ_BATCH, 31]);
    assert!(
        got.iter()
            .enumerate()
            .all(|(index, byte)| *byte == pattern_byte(index))
    );
}
