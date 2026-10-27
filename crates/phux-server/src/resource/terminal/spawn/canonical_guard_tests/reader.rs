//! Reader-side real-PTY tests: output coalescing, the quiet poller, fd
//! inheritance, and the inline write path.

use super::*;

/// A flood larger than the batch cap comes back intact, in order, and no
/// handoff is bigger than the cap.
///
/// How many kernel reads one handoff folds depends on how far `cat` got
/// ahead of the reader, which a real PTY does not fix; the scripted
/// sources in `park`'s tests prove the folding itself.
#[tokio::test(flavor = "current_thread")]
async fn coalesced_pty_output_keeps_order_and_respects_the_cap() {
    let (pty_rx, input_tx, mut pty) =
        spawn_pty(CommandBuilder::new("cat"), 80, 24).expect("spawn cat under a real pty");
    set_raw(&pty);
    let len = PTY_READ_BATCH + 4_096 + 17;
    let payload: Vec<u8> = (0..len)
        .map(|index| u8::try_from(index % 251).expect("byte"))
        .collect();
    // Drain while the write is in flight. The reader channel is bounded;
    // waiting to read until the write returns deadlocks once the payload
    // no longer fits in that channel plus the kernel buffer.
    let expected = payload.len();
    let reader = std::thread::spawn(move || {
        let mut pty_rx = pty_rx;
        let mut received = Vec::new();
        let mut chunks = 0_usize;
        let mut max_chunk = 0_usize;
        let deadline = std::time::Instant::now() + DELIVERY_DEADLINE;
        while received.len() < expected && std::time::Instant::now() < deadline {
            match pty_rx.try_recv() {
                Ok(PtyEvent::Bytes { chunk, .. }) => {
                    assert!(!chunk.is_empty(), "empty handoff");
                    max_chunk = max_chunk.max(chunk.len());
                    received.extend_from_slice(&chunk);
                    chunks += 1;
                }
                Ok(PtyEvent::Eof) | Err(mpsc::error::TryRecvError::Disconnected) => break,
                Err(mpsc::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
        (received, chunks, max_chunk)
    });
    assert_eq!(
        write_and_wait(&input_tx, payload.clone()).await,
        WriteCompletion::Delivered
    );
    let (received, chunks, max_chunk) = reader.join().expect("reader thread");
    assert!(
        max_chunk <= PTY_READ_BATCH,
        "handoff of {max_chunk} bytes exceeds the cap"
    );
    assert_eq!(received, payload, "pty bytes were lost or reordered");
    assert!(
        chunks >= 2,
        "a payload larger than the cap must not arrive as one unlimited read"
    );
    let _ = pty.child.kill();
}

/// One echoed byte is its own handoff. Waiting to fill the cap would
/// block here, because nothing else is written.
#[tokio::test(flavor = "current_thread")]
async fn one_pty_byte_is_delivered_without_waiting_for_a_full_batch() {
    let (mut pty_rx, input_tx, mut pty) =
        spawn_pty(CommandBuilder::new("cat"), 80, 24).expect("spawn cat under a real pty");
    set_raw(&pty);
    assert_eq!(
        write_and_wait(&input_tx, b"Z".to_vec()).await,
        WriteCompletion::Delivered
    );
    let event = tokio::time::timeout(Duration::from_millis(500), pty_rx.recv())
        .await
        .expect("a single byte must be handed off without waiting for more")
        .expect("pty output channel open");
    match event {
        PtyEvent::Bytes { chunk, .. } => assert_eq!(&chunk[..], b"Z"),
        PtyEvent::Eof => panic!("pane closed before the byte arrived"),
    }
    let extra = tokio::time::timeout(Duration::from_millis(50), pty_rx.recv()).await;
    assert!(extra.is_err(), "no further byte was written");
    let _ = pty.child.kill();
}

/// Collect pane output until `done` holds for everything received, the
/// pane ends, or nothing arrives for [`DELIVERY_DEADLINE`].
///
/// Completion is the expected output, never a quiet spell: dropping a
/// pane's input writes a newline and `VEOF`, whose echo is a whole line
/// that lands before a slow-starting child prints anything.
fn collect_until(rx: &mut mpsc::Receiver<PtyEvent>, done: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let mut got = Vec::new();
    let mut last_progress = std::time::Instant::now();
    while last_progress.elapsed() < DELIVERY_DEADLINE {
        match rx.try_recv() {
            Ok(PtyEvent::Bytes { chunk, .. }) => {
                got.extend_from_slice(&chunk);
                last_progress = std::time::Instant::now();
                if done(&got) {
                    break;
                }
            }
            Ok(PtyEvent::Eof) | Err(mpsc::error::TryRecvError::Disconnected) => break,
            Err(mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    got
}

/// Whether `got` holds a whole line that contains `marker`.
fn has_line_with(got: &[u8], marker: &[u8]) -> bool {
    got.windows(marker.len())
        .position(|window| window == marker)
        .is_some_and(|at| got[at..].contains(&b'\n'))
}

/// Idle panes sit on the shared poller. They must not each grow a reader thread.
///
/// Each seat is this test's own reader. A process-global tally also moves
/// when another test parks a pane, and when a previous test's hot reader
/// drops its slot only after that thread notices cancel (phux-x5aqg).
#[test]
fn idle_panes_share_one_poller() {
    use crate::resource::terminal::park::{ReaderPlace, reader_place};
    let mut live = Vec::new();
    for _ in 0..8 {
        let mut cmd = CommandBuilder::new("sleep");
        cmd.arg("60");
        live.push(spawn_pty(cmd, 80, 24).expect("spawn sleep"));
    }
    let places: Vec<_> = live
        .iter()
        .map(|(_, _, pty)| reader_place(&pty.reader))
        .collect();
    let parked = places
        .iter()
        .filter(|place| **place == ReaderPlace::Parked)
        .count();
    let hot = places
        .iter()
        .filter(|place| matches!(*place, ReaderPlace::Hot | ReaderPlace::Dedicated))
        .count();
    assert_eq!(
        (parked, hot),
        (8, 0),
        "idle panes share the one poller; places {places:?}"
    );
    for (_, _, mut pty) in live {
        let _ = pty.child.kill();
    }
}

/// Service, shell and direct-command panes expose the same colour capability.
#[test]
fn pane_children_always_advertise_truecolor() {
    for inherited in [None, Some(""), Some("24bit"), Some("256color")] {
        let mut cmd = shell_command(
            "/bin/sh",
            "printf 'COLOR:%s TERM:%s\\n' \"$COLORTERM\" \"$TERM\"",
            false,
        );
        match inherited {
            Some(value) => cmd.env("COLORTERM", value),
            None => cmd.env_remove("COLORTERM"),
        }
        let (mut rx, _, mut pty) = spawn_pty(cmd, 80, 24).expect("spawn color probe");
        let got = collect_until(&mut rx, |got| has_line_with(got, b"COLOR:"));
        let _ = pty.child.wait();
        let text = String::from_utf8_lossy(&got);
        assert!(
            text.contains("COLOR:truecolor TERM:xterm-256color"),
            "{inherited:?}: {text}"
        );
    }
}

/// The first burst promotes the pane onto a dedicated reader.
#[test]
fn output_promotes_a_pane_off_the_poller() {
    let before = crate::resource::terminal::park::promote_count();
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg("printf 'hello-from-pty\\n'; sleep 30");
    let (mut rx, _, mut pty) = spawn_pty(cmd, 80, 24).expect("spawn a pane that prints once");
    let got = collect_until(&mut rx, |got| has_line_with(got, b"hello-from-pty"));
    assert!(
        got.windows(14).any(|window| window == b"hello-from-pty"),
        "missing prompt bytes in {got:?}"
    );
    // The byte is queued before the poller records the promotion.
    let deadline = std::time::Instant::now() + DELIVERY_DEADLINE;
    while crate::resource::terminal::park::promote_count() <= before
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        crate::resource::terminal::park::promote_count() > before,
        "a pane that produced output should leave the shared poller"
    );
    let _ = pty.child.kill();
}

/// A high fd held open in the server must not be inherited, and on Linux
/// the child's allocated fd table must collapse back down.
#[test]
fn pane_child_does_not_inherit_the_servers_fd_table() {
    use std::os::fd::FromRawFd;
    let devnull = std::fs::File::open("/dev/null").expect("devnull");
    let high =
        nix::fcntl::fcntl(&devnull, nix::fcntl::FcntlArg::F_DUPFD(4_000)).expect("dup a high fd");
    let probe = format!(
        "result=CLEAN; \
         if [ -e /dev/fd/{high} ]; then result=LEAK; fi; \
         open=0; i=0; \
         while [ \"$i\" -lt 64 ]; do \
           if [ -e \"/dev/fd/$i\" ]; then open=$((open + 1)); fi; \
           i=$((i + 1)); \
         done; \
         fdsize=na; \
         if [ -r /proc/self/status ]; then \
           fdsize=$(awk '/^FDSize:/ {{ print $2 }}' /proc/self/status); \
         fi; \
         printf '%s COUNT:%s FDSize:%s\\n' \"$result\" \"$open\" \"$fdsize\""
    );
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg(probe);
    let (mut rx, _, mut pty) = spawn_pty(cmd, 80, 24).expect("spawn probe");
    let got = collect_until(&mut rx, |got| has_line_with(got, b"FDSize:"));
    let text = String::from_utf8_lossy(&got);
    assert!(text.contains("CLEAN"), "child inherited fd {high}: {text}");
    let count = text.split_whitespace().find_map(|field| {
        field
            .strip_prefix("COUNT:")
            .and_then(|value| value.parse::<u64>().ok())
    });
    let count = count.expect("COUNT line");
    assert!(
        count < 16,
        "child kept {count} fds below 64; the spawn should not copy the server table: {text}"
    );
    #[cfg(target_os = "linux")]
    {
        let fdsize = text.split_whitespace().find_map(|field| {
            field
                .strip_prefix("FDSize:")
                .and_then(|value| value.parse::<u64>().ok())
        });
        let fdsize = fdsize.expect("FDSize line");
        assert!(
            fdsize < 512,
            "fd table stayed fat (FDSize {fdsize}) after the trampoline"
        );
    }
    let _ = pty.child.kill();
    // SAFETY: `fcntl` just created `high` and this test owns it.
    drop(unsafe { std::os::fd::OwnedFd::from_raw_fd(high) });
}

/// Inline input must not consume output ahead of the authoritative reader.
/// Otherwise the reader can hold an earlier chunk while a direct write
/// enqueues a later one first, assigning valid sequences to reordered bytes.
#[test]
fn inline_pty_write_leaves_pending_output_for_the_authoritative_reader() {
    use crate::resource::terminal::fast_pty::{DirectWrite, PtyFastPath};
    use std::os::fd::AsFd;

    let (master, _slave) = open_default_pty();
    let mut reader = dup_master_reader(&*master).expect("reader");
    crate::resource::terminal::park::set_nonblock(&reader).expect("nonblocking reader");
    let mut writer = master.take_writer().expect("writer");
    let prefix = b"before";
    writer.write_all(prefix).expect("queue the earlier echo");
    let mut ready = [nix::poll::PollFd::new(
        reader.as_fd(),
        nix::poll::PollFlags::POLLIN,
    )];
    assert_eq!(
        nix::poll::poll(
            &mut ready,
            nix::poll::PollTimeout::try_from(DELIVERY_DEADLINE).expect("deadline")
        )
        .expect("wait for pending output"),
        1,
        "the prefix must already be readable before the inline write"
    );
    let (input_tx, input_rx) = mpsc::channel(1);
    let fast = PtyFastPath::new(input_tx, Arc::new(Mutex::new(master)), writer);

    assert_eq!(fast.try_direct(b"after"), DirectWrite::Wrote);
    let mut actual = [0; 64];
    let n = reader
        .read(&mut actual)
        .expect("the authoritative reader retains the prefix");
    assert!(
        actual[..n].starts_with(prefix),
        "output reordered: {:?}",
        &actual[..n]
    );

    fast.close();
    fast.run_writer(input_rx);
}

/// Enter and Ctrl-U are not legacy printables, so they sit in the input
/// lane while `pending` is still zero. The next letter must not write
/// ahead of them.
#[test]
fn an_in_flight_lane_key_blocks_the_inline_pty_write() {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg("cat");
    let (_pty_rx, _input_tx, mut pty) = spawn_pty(cmd, 80, 24).expect("spawn");
    let guard = crate::resource::terminal::fast_pty::LaneAhead::arm(&pty.fast);
    assert_eq!(
        pty.fast.try_direct(b"e"),
        crate::resource::terminal::fast_pty::DirectWrite::Busy,
        "a printable must not pass a key the lane has not enqueued"
    );
    drop(guard);
    assert_eq!(
        pty.fast.try_direct(b"e"),
        crate::resource::terminal::fast_pty::DirectWrite::Wrote
    );
    let _ = pty.child.kill();
}
