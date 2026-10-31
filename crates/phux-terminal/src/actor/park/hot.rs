//! The hot and dedicated reader loops.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

#[allow(
    clippy::needless_pass_by_value,
    reason = "the reader thread keeps the cancel flag alive after its slot is removed"
)]
pub(in crate::actor) fn hot_loop(
    park: &'static Park,
    id: u64,
    mut file: File,
    mut tx: mpsc::Sender<PtyEvent>,
    cancel: std::sync::Arc<AtomicBool>,
    pending: Vec<PtyEvent>,
) {
    if !flush_pending(&tx, &mut file, pending) {
        park.remove(id);
        return;
    }
    let mut bufs = CoalesceBufs::new();
    let mut last = Instant::now();
    loop {
        if cancel.load(Ordering::Acquire) {
            park.remove(id);
            return;
        }
        let idle = last.elapsed();
        if idle >= QUIET_AFTER {
            match try_repark(park, id, file, tx) {
                Repark::Parked => return,
                Repark::Keep(kept_file, kept_tx) => {
                    file = kept_file;
                    tx = kept_tx;
                    last = Instant::now();
                }
            }
        }
        let slice = CANCEL_SLICE.min(QUIET_AFTER.saturating_sub(idle));
        match poll_readable(&file, slice) {
            PollRead::Timeout => {}
            PollRead::Ready => {
                if handle_hot_read(park, id, &mut file, &tx, &mut bufs, &mut last).is_exit() {
                    return;
                }
            }
            PollRead::Hungup => {
                let _ = tx.blocking_send(PtyEvent::Eof);
                park.remove(id);
                return;
            }
        }
    }
}

pub(in crate::actor) enum AfterRead {
    Stay,
    Exit,
}

impl AfterRead {
    pub(in crate::actor) const fn is_exit(&self) -> bool {
        matches!(self, Self::Exit)
    }
}

pub(in crate::actor) fn handle_hot_read(
    park: &'static Park,
    id: u64,
    file: &mut File,
    tx: &mpsc::Sender<PtyEvent>,
    bufs: &mut CoalesceBufs,
    last: &mut Instant,
) -> AfterRead {
    match gather_wake(&mut MasterFile { file }, &mut bufs.scratch, &mut bufs.batch) {
        Gathered::Pending => AfterRead::Stay,
        Gathered::Eof => finish_hot(park, id, tx, PtyEvent::Eof),
        Gathered::Bytes { eof } => {
            *last = Instant::now();
            let chunk = bufs.batch.split().freeze();
            if send_pty_chunk(tx, chunk, *last).is_break() {
                drain_file_to_eof(file, &mut bufs.scratch);
                park.remove(id);
                return AfterRead::Exit;
            }
            if eof {
                finish_hot(park, id, tx, PtyEvent::Eof)
            } else {
                AfterRead::Stay
            }
        }
    }
}

pub(in crate::actor) fn finish_hot(
    park: &Park,
    id: u64,
    tx: &mpsc::Sender<PtyEvent>,
    event: PtyEvent,
) -> AfterRead {
    let _ = tx.blocking_send(event);
    park.remove(id);
    AfterRead::Exit
}

/// `false` when the actor is gone and the master has been drained.
pub(in crate::actor) fn flush_pending(
    tx: &mpsc::Sender<PtyEvent>,
    file: &mut File,
    pending: Vec<PtyEvent>,
) -> bool {
    for event in pending {
        match event {
            PtyEvent::Bytes { chunk, read_at } => {
                if send_pty_chunk(tx, chunk, read_at).is_break() {
                    let mut buf = vec![0_u8; PTY_READ_CHUNK];
                    drain_file_to_eof(file, &mut buf);
                    return false;
                }
            }
            PtyEvent::Eof => {
                let _ = tx.blocking_send(PtyEvent::Eof);
                return false;
            }
        }
    }
    true
}

pub(in crate::actor) enum Repark {
    /// Fd is on the poller, or the pane is gone. The reader thread exits.
    Parked,
    /// Stay on this thread; the poller refused the fd.
    Keep(File, mpsc::Sender<PtyEvent>),
}

/// Park `file`. On cancel the slot is removed and `file` is dropped here.
pub(in crate::actor) fn try_repark(
    park: &'static Park,
    id: u64,
    file: File,
    tx: mpsc::Sender<PtyEvent>,
) -> Repark {
    let mut slots = park.slots();
    let Some(slot) = slots.get_mut(&id) else {
        return Repark::Parked;
    };
    if slot.cancel.load(Ordering::Acquire) {
        park.reactor.delete(file.as_fd());
        slots.remove(&id);
        return Repark::Parked;
    }
    if let Err(err) = park.reactor.add(file.as_fd(), id) {
        tracing::warn!(?err, "returning a quiet pty to the poller failed");
        return Repark::Keep(file, tx);
    }
    slot.join.take();
    slot.state = SlotState::Parked(ParkedPty { file, tx });
    drop(slots);
    park.poke();
    Repark::Parked
}

pub(in crate::actor) enum PollRead {
    Timeout,
    Ready,
    Hungup,
}

pub(in crate::actor) fn poll_readable(file: &File, wait: Duration) -> PollRead {
    let millis = u16::try_from(wait.as_millis()).unwrap_or(u16::MAX);
    let mut fds = [nix::poll::PollFd::new(
        file.as_fd(),
        nix::poll::PollFlags::POLLIN,
    )];
    match nix::poll::poll(&mut fds, nix::poll::PollTimeout::from(millis)) {
        Ok(0) | Err(nix::errno::Errno::EINTR) => PollRead::Timeout,
        Ok(_) => match fds[0].revents() {
            Some(flags)
                if flags.contains(nix::poll::PollFlags::POLLHUP)
                    && !flags.contains(nix::poll::PollFlags::POLLIN) =>
            {
                PollRead::Hungup
            }
            Some(flags)
                if flags.intersects(
                    nix::poll::PollFlags::POLLIN
                        | nix::poll::PollFlags::POLLHUP
                        | nix::poll::PollFlags::POLLERR
                        | nix::poll::PollFlags::POLLNVAL,
                ) =>
            {
                PollRead::Ready
            }
            _ => PollRead::Timeout,
        },
        Err(err) => {
            tracing::debug!(?err, "pty reader poll failed");
            PollRead::Hungup
        }
    }
}

pub(in crate::actor) fn spawn_dedicated(file: File, tx: mpsc::Sender<PtyEvent>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("phux-pty-reader".to_owned())
        .stack_size(READER_STACK)
        .spawn(move || {
            let mut file = file;
            dedicated_loop(&mut file, &tx);
        })
        .unwrap_or_else(|err| {
            tracing::error!(?err, "pty reader thread failed to start");
            // A joined dummy keeps `PtyReader`'s shutdown path uniform.
            std::thread::spawn(|| {})
        })
}

pub(in crate::actor) fn dedicated_loop(file: &mut File, tx: &mpsc::Sender<PtyEvent>) {
    phux_resource::perf::promote_helper_thread("phux-pty-reader");
    let mut bufs = CoalesceBufs::new();
    loop {
        match gather_wake(&mut MasterFile { file }, &mut bufs.scratch, &mut bufs.batch) {
            // The nonblocking master is empty; wait for the next wake
            // instead of spinning.
            Gathered::Pending => {
                if matches!(poll_readable(file, CANCEL_SLICE), PollRead::Hungup) {
                    let _ = tx.blocking_send(PtyEvent::Eof);
                    break;
                }
            }
            Gathered::Eof => {
                let _ = tx.blocking_send(PtyEvent::Eof);
                break;
            }
            Gathered::Bytes { eof } => {
                let chunk = bufs.batch.split().freeze();
                if send_pty_chunk(tx, chunk, Instant::now()).is_break() {
                    drain_file_to_eof(file, &mut bufs.scratch);
                    break;
                }
                if eof {
                    let _ = tx.blocking_send(PtyEvent::Eof);
                    break;
                }
            }
        }
    }
}

pub(in crate::actor) fn set_cloexec_nonblock(fd: &OwnedFd) -> io::Result<()> {
    nix::fcntl::fcntl(
        fd,
        nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
    )
    .map_err(io::Error::from)?;
    let flags = nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFL).map_err(io::Error::from)?;
    let mut flags = nix::fcntl::OFlag::from_bits_retain(flags);
    flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
    nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_SETFL(flags)).map_err(io::Error::from)?;
    Ok(())
}

pub(in crate::actor) fn drain_fd(fd: &OwnedFd) {
    let mut buf = [0_u8; 64];
    loop {
        match nix::unistd::read(fd, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}
