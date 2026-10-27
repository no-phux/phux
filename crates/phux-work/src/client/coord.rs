//! Mail and claims (ADR-0187) over the same [`Client`].

use super::{Client, Patience, REPLY_TIMEOUT, unexpected};
use crate::coord::{
    AckMail, Claim, ClaimFilter, ClaimOutcome, Mail, ReadMail, ReleaseClaim, ReleaseMail,
    RenewClaim, SendMail, Sent, TakeClaim, WaitMail,
};
use crate::error::Error;
use crate::wire::{Request, Response};
use std::time::Duration;

impl Client {
    /// Send a message. See [`crate::Store::send_mail`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn send_mail(&self, request: SendMail) -> Result<Sent, Error> {
        match self.call(Request::SendMail(request))? {
            Response::Sent(sent) => Ok(sent),
            other => Err(unexpected(&other)),
        }
    }

    /// A reader's mail. See [`crate::Store::read_mail`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn read_mail(&self, request: ReadMail) -> Result<Vec<Mail>, Error> {
        match self.call(Request::ReadMail(request))? {
            Response::Mail(mail) => Ok(mail),
            other => Err(unexpected(&other)),
        }
    }

    /// Acknowledge a message. See [`crate::Store::ack_mail`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`], plus [`Error::NoMail`].
    pub fn ack_mail(&self, request: AckMail) -> Result<Mail, Error> {
        match self.call(Request::AckMail(request))? {
            Response::Acked(mail) => Ok(mail),
            other => Err(unexpected(&other)),
        }
    }

    /// Take the reader's undelivered mail, waiting up to the request's
    /// timeout for some to arrive. The keeper holds the request open; an
    /// empty result means the deadline passed. See [`crate::Store::take_mail`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn wait_mail(&self, request: WaitMail) -> Result<Vec<Mail>, Error> {
        self.wait_mail_bound(request, None)
    }

    /// [`Client::wait_mail`], abandoned without taking anything as soon as
    /// `lifeline` (the read end of a pipe the caller's parent holds open)
    /// closes. A wait whose parent died cannot take the next message.
    ///
    /// # Errors
    ///
    /// As [`Client::add`], and [`Error::Keeper`] once the lifeline closes.
    pub fn wait_mail_while(
        &self,
        request: WaitMail,
        lifeline: std::os::fd::BorrowedFd<'_>,
    ) -> Result<Vec<Mail>, Error> {
        self.wait_mail_bound(request, Some(lifeline))
    }

    fn wait_mail_bound(
        &self,
        request: WaitMail,
        lifeline: Option<std::os::fd::BorrowedFd<'_>>,
    ) -> Result<Vec<Mail>, Error> {
        let patience = Patience {
            reply: Duration::from_millis(request.timeout_ms).saturating_add(REPLY_TIMEOUT),
            lifeline,
        };
        match self.call_within(Request::WaitMail(request), patience)? {
            Response::Mail(mail) => Ok(mail),
            other => Err(unexpected(&other)),
        }
    }

    /// Return taken mail to the reader's undelivered mail. See
    /// [`crate::Store::release_mail`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn release_mail(&self, request: ReleaseMail) -> Result<u64, Error> {
        match self.call(Request::ReleaseMail(request))? {
            Response::Released(count) => Ok(count),
            other => Err(unexpected(&other)),
        }
    }

    /// Acquire a new lease. See [`crate::Store::take_claim`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn take_claim(&self, request: TakeClaim) -> Result<ClaimOutcome, Error> {
        match self.call(Request::TakeClaim(request))? {
            Response::ClaimOutcome(outcome) => Ok(outcome),
            other => Err(unexpected(&other)),
        }
    }

    /// Renew a live lease. See [`crate::Store::renew_claim`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`], or [`Error::NoClaim`] for a stale receipt.
    pub fn renew_claim(&self, request: RenewClaim) -> Result<Vec<Claim>, Error> {
        match self.call(Request::RenewClaim(request))? {
            Response::Claims(claims) => Ok(claims),
            other => Err(unexpected(&other)),
        }
    }

    /// Release claims. See [`crate::Store::release_claim`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn release_claim(&self, request: ReleaseClaim) -> Result<u64, Error> {
        match self.call(Request::ReleaseClaim(request))? {
            Response::Released(count) => Ok(count),
            other => Err(unexpected(&other)),
        }
    }

    /// Live claims. See [`crate::Store::claims`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn claims(&self, filter: ClaimFilter) -> Result<Vec<Claim>, Error> {
        match self.call(Request::Claims(filter))? {
            Response::Claims(claims) => Ok(claims),
            other => Err(unexpected(&other)),
        }
    }
}
