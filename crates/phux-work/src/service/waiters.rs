//! Requests parked until mail arrives for their reader.
//!
//! The keeper serves one connection at a time, so a wait cannot block it.
//! Instead the waiting connection is kept here, polled only for hang-up, and
//! answered when a send may have given it mail, when its deadline passes,
//! or never if its client leaves first. Taking marks what it returns, so two
//! waiters for one reader never both receive a message. A take whose reply
//! cannot be written is released at once: mail provably not handed over is
//! never counted as delivered.

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use super::reply;
use crate::coord::{Mail, ReleaseMail, WaitMail};
use crate::error::Error;
use crate::store::Store;
use crate::wire::Response;

struct Waiter {
    stream: UnixStream,
    wait: WaitMail,
    deadline: Instant,
}

/// Every parked wait, oldest first.
#[derive(Default)]
pub(super) struct Waiters(Vec<Waiter>);

impl Waiters {
    /// Answer `wait` now when it has mail, fails, or does not wait;
    /// otherwise park it.
    pub(super) fn admit(&mut self, stream: &UnixStream, wait: WaitMail, store: &mut Store) {
        match store.take_mail(&wait) {
            Ok(mail) if mail.is_empty() && wait.timeout_ms > 0 => match stream.try_clone() {
                Ok(stream) => self.0.push(Waiter {
                    deadline: Instant::now() + Duration::from_millis(wait.timeout_ms),
                    stream,
                    wait,
                }),
                Err(err) => {
                    let _ = reply(stream, &Response::Fault(Error::from(err).into()));
                }
            },
            result => hand_over(stream, &wait.reader, result, store),
        }
    }

    /// Answer every waiter that now has mail, in the order they arrived.
    pub(super) fn wake(&mut self, store: &mut Store) {
        self.0.retain(|waiter| match store.take_mail(&waiter.wait) {
            Ok(mail) if mail.is_empty() => true,
            result => {
                hand_over(&waiter.stream, &waiter.wait.reader, result, store);
                false
            }
        });
    }

    /// Answer every waiter whose deadline has passed with no mail.
    pub(super) fn expire(&mut self, now: Instant) {
        self.0.retain(|waiter| {
            if waiter.deadline > now {
                return true;
            }
            let _ = reply(&waiter.stream, &Response::Mail(Vec::new()));
            false
        });
    }

    /// Drop the waiters at `gone` (indices into [`Self::streams`]), whose
    /// clients hung up. Nothing was taken for them.
    pub(super) fn forget(&mut self, gone: &[usize]) {
        let mut index = 0;
        self.0.retain(|_| {
            let keep = !gone.contains(&index);
            index += 1;
            keep
        });
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.0.iter().map(|waiter| waiter.deadline).min()
    }

    pub(super) const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn streams(&self) -> impl Iterator<Item = &UnixStream> {
        self.0.iter().map(|waiter| &waiter.stream)
    }
}

/// Reply with what a take returned, and release it if the reply is lost.
fn hand_over(
    stream: &UnixStream,
    reader: &str,
    result: Result<Vec<Mail>, Error>,
    store: &mut Store,
) {
    let mail = match result {
        Ok(mail) => mail,
        Err(err) => {
            let _ = reply(stream, &Response::Fault(err.into()));
            return;
        }
    };
    let ids = mail.iter().map(|message| message.id).collect();
    if reply(stream, &Response::Mail(mail)).is_err() {
        // The release is best effort: a store failure here leaves the mail
        // unread in the inbox, never lost.
        let _ = store.release_mail(&ReleaseMail {
            reader: reader.to_owned(),
            ids,
        });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

    use std::io::Write as _;
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    use crate::coord::{SendMail, WaitMail};
    use crate::id::OperationId;
    use crate::service::{Endpoint, run_keeper};
    use crate::wire::{BUILD, Envelope, Request};
    use crate::{Client, KeeperExit, Store};

    #[test]
    fn a_waiter_that_hangs_up_takes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::new(&root.path().join("state"), &root.path().join("run"));
        let keeper = {
            let endpoint = endpoint.clone();
            std::thread::spawn(move || run_keeper(&endpoint, Duration::from_millis(300)).unwrap())
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut gone = loop {
            match UnixStream::connect(&endpoint.socket) {
                Ok(stream) => break stream,
                Err(_) => assert!(Instant::now() < deadline, "keeper never bound"),
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let wait = |timeout_ms| WaitMail {
            reader: "@730".into(),
            timeout_ms,
            limit: 0,
        };
        let line = serde_json::to_string(&Envelope {
            build: BUILD.to_owned(),
            request: Request::WaitMail(wait(30_000)),
        })
        .unwrap();
        gone.write_all(format!("{line}\n").as_bytes()).unwrap();
        drop(gone);
        // The keeper accepts in order and reports a parked hang-up before a
        // new connection, so the send below finds the waiter already gone.
        let client = Client::new(endpoint, None);
        client
            .send_mail(SendMail {
                thread: "t".into(),
                from: "a".into(),
                to: vec!["@730".into()],
                body: "kept".into(),
                operation_id: OperationId::generate().unwrap(),
            })
            .unwrap();
        let mail = client.wait_mail(wait(0)).unwrap();
        assert_eq!(mail.len(), 1, "the message was not spent on a dead client");
        assert_eq!(keeper.join().unwrap(), KeeperExit::Idle);
    }

    #[test]
    fn a_take_whose_reply_cannot_be_written_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("work").join("work-v1.sqlite")).unwrap();
        store
            .send_mail(&SendMail {
                thread: "t".into(),
                from: "a".into(),
                to: vec!["@730".into()],
                body: "kept".into(),
                operation_id: OperationId::generate().unwrap(),
            })
            .unwrap();
        let (keeper_end, client_end) = UnixStream::pair().unwrap();
        drop(client_end);
        let wait = WaitMail {
            reader: "@730".into(),
            timeout_ms: 0,
            limit: 0,
        };
        super::Waiters::default().admit(&keeper_end, wait.clone(), &mut store);
        assert_eq!(
            store.take_mail(&wait).unwrap().len(),
            1,
            "the lost reply's take was released"
        );
    }
}
