//! Mail and claims between a hub and its satellites.
//!
//! A satellite's coordination lives in its hub's store, so an agent in a box
//! and an agent on the hub read the same mail and see the same claims. The
//! satellite's client sends each call over its link to the hub
//! ([`crate::Exchange`]); the hub answers it with a [`Gateway`] for that
//! satellite, which the hub names from the satellite's credential, never
//! from the request.
//!
//! **Addresses are relative.** On the satellite, `@3` is its own pane and
//! `hub/@749` is the hub's; on the hub, the same two are `min-api/@3` and
//! `@749`. The gateway rewrites one into the other both ways, so each side
//! writes and reads addresses as it would at home, and another satellite's
//! `min-web/@2` means the same thing on both.
//!
//! **A satellite speaks only as itself.** Every sender, reader, and holder
//! it names is put under its own name, so it cannot read, take, or ack the
//! hub's mail, or renew or release a claim the hub holds. It may write to
//! anyone, and it may see every claim, as any agent on the hub may.

use std::os::fd::BorrowedFd;

use crate::client::Client;
use crate::coord::{
    AckMail, Address, Claim, ClaimFilter, ClaimOutcome, Mail, ReadMail, ReleaseClaim, ReleaseMail,
    RenewClaim, SendMail, Sent, TakeClaim, WaitMail,
};
use crate::error::Error;
use crate::wire::{BUILD, Envelope, Fault, Request, Response};

/// The host part that names the hub itself, from a satellite: `hub/@749`.
pub const HUB: &str = "hub";

/// Answers one satellite's coordination calls against this hub's store.
#[derive(Debug, Clone)]
pub struct Gateway {
    client: Client,
    satellite: String,
}

impl Gateway {
    /// Answer `satellite`'s calls with `client`, this hub's store.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidText`] when `satellite` cannot lead an address: it is
    /// `hub`, or uses a character outside `[A-Za-z0-9._-]`.
    pub fn new(client: Client, satellite: &str) -> Result<Self, Error> {
        let probe = Address::parse(&format!("{satellite}/@1"))?;
        if probe.host() != Some(satellite) {
            return Err(Error::invalid_text(
                "satellite",
                "named `hub` cannot carry mail; rename it",
            ));
        }
        Ok(Self {
            client,
            satellite: satellite.to_owned(),
        })
    }

    /// The reply line to one request line from the satellite. A wait gives
    /// up, taking nothing, once `lifeline` closes.
    #[must_use]
    pub fn answer(&self, line: &str, lifeline: Option<BorrowedFd<'_>>) -> String {
        let response = match serde_json::from_str::<Envelope>(line) {
            Ok(envelope) => self
                .dispatch(envelope.request, lifeline)
                .unwrap_or_else(|err| Response::Fault(err.into())),
            Err(err) => Response::Fault(Fault::BadRequest {
                message: err.to_string(),
                keeper: Some(BUILD.to_owned()),
            }),
        };
        let mut reply = serde_json::to_string(&response).unwrap_or_else(|err| {
            format!(r#"{{"result":"fault","value":{{"kind":"store","message":"{err}"}}}}"#)
        });
        reply.push('\n');
        reply
    }

    fn dispatch(
        &self,
        request: Request,
        lifeline: Option<BorrowedFd<'_>>,
    ) -> Result<Response, Error> {
        match request {
            Request::SendMail(send) => self.send_mail(send),
            Request::ReadMail(read) => self.read_mail(read),
            Request::AckMail(ack) => self.ack_mail(ack),
            Request::WaitMail(wait) => self.wait_mail(wait, lifeline),
            Request::ReleaseMail(release) => self.release_mail(release),
            Request::TakeClaim(take) => self.take_claim(take),
            Request::RenewClaim(renew) => self.renew_claim(renew),
            Request::ReleaseClaim(release) => self.release_claim(release),
            Request::Claims(filter) => self.list_claims(filter),
            _ => Err(Error::invalid_text(
                "request",
                "is not mail or a claim, the only calls a satellite makes on its hub",
            )),
        }
    }

    fn send_mail(&self, mut send: SendMail) -> Result<Response, Error> {
        send.from = self.own(&send.from)?;
        send.to = send
            .to
            .iter()
            .map(|to| self.inward(to))
            .collect::<Result<_, _>>()?;
        Ok(Response::Sent(self.sent(self.client.send_mail(send)?)))
    }

    fn read_mail(&self, mut read: ReadMail) -> Result<Response, Error> {
        read.reader = self.own(&read.reader)?;
        Ok(Response::Mail(self.mail(self.client.read_mail(read)?)))
    }

    fn ack_mail(&self, mut ack: AckMail) -> Result<Response, Error> {
        ack.reader = self.own(&ack.reader)?;
        Ok(Response::Acked(self.message(self.client.ack_mail(ack)?)))
    }

    fn release_mail(&self, mut release: ReleaseMail) -> Result<Response, Error> {
        release.reader = self.own(&release.reader)?;
        Ok(Response::Released(self.client.release_mail(release)?))
    }

    fn renew_claim(&self, mut renew: RenewClaim) -> Result<Response, Error> {
        renew.holder = self.own(&renew.holder)?;
        Ok(Response::Claims(
            self.claims(self.client.renew_claim(renew)?),
        ))
    }

    fn release_claim(&self, mut release: ReleaseClaim) -> Result<Response, Error> {
        release.holder = self.own(&release.holder)?;
        Ok(Response::Released(self.client.release_claim(release)?))
    }

    fn wait_mail(
        &self,
        mut wait: WaitMail,
        lifeline: Option<BorrowedFd<'_>>,
    ) -> Result<Response, Error> {
        wait.reader = self.own(&wait.reader)?;
        let taken = match lifeline {
            Some(lifeline) => self.client.wait_mail_while(wait, lifeline)?,
            None => self.client.wait_mail(wait)?,
        };
        Ok(Response::Mail(self.mail(taken)))
    }

    fn take_claim(&self, mut take: TakeClaim) -> Result<Response, Error> {
        take.holder = self.own(&take.holder)?;
        Ok(Response::ClaimOutcome(
            match self.client.take_claim(take)? {
                ClaimOutcome::Granted(claims) => ClaimOutcome::Granted(self.claims(claims)),
                ClaimOutcome::Conflict(claims) => ClaimOutcome::Conflict(self.claims(claims)),
            },
        ))
    }

    fn list_claims(&self, mut filter: ClaimFilter) -> Result<Response, Error> {
        filter.holder = filter
            .holder
            .map(|holder| self.inward(&holder))
            .transpose()?;
        Ok(Response::Claims(self.claims(self.client.claims(filter)?)))
    }

    /// An address the satellite speaks as, put under its name.
    fn own(&self, address: &str) -> Result<String, Error> {
        let parsed = Address::parse(address)?;
        match parsed.host() {
            Some(host) if host == self.satellite => Ok(parsed.display().to_owned()),
            None if !parsed.display().contains('/') => {
                Ok(format!("{}/{}", self.satellite, parsed.display()))
            }
            _ => Err(Error::invalid_text(
                "address",
                "from a satellite may only be its own, such as `@3`",
            )),
        }
    }

    /// An address as the satellite wrote it, as the hub reads it.
    fn inward(&self, address: &str) -> Result<String, Error> {
        let parsed = Address::parse(address)?;
        Ok(match parsed.display().split_once('/') {
            Some((HUB, local)) => local.to_owned(),
            Some(_) => parsed.display().to_owned(),
            None => format!("{}/{}", self.satellite, parsed.display()),
        })
    }

    /// An address as the hub stored it, as the satellite reads it.
    fn outward(&self, address: &str) -> String {
        match address.split_once('/') {
            Some((host, local)) if host == self.satellite => local.to_owned(),
            Some(_) => address.to_owned(),
            None => format!("{HUB}/{address}"),
        }
    }

    fn message(&self, mut mail: Mail) -> Mail {
        mail.from = self.outward(&mail.from);
        for to in &mut mail.to {
            *to = self.outward(to);
        }
        mail
    }

    fn mail(&self, mail: Vec<Mail>) -> Vec<Mail> {
        mail.into_iter().map(|mail| self.message(mail)).collect()
    }

    fn sent(&self, sent: Sent) -> Sent {
        Sent {
            mail: self.message(sent.mail),
            ..sent
        }
    }

    fn claims(&self, claims: Vec<Claim>) -> Vec<Claim> {
        claims
            .into_iter()
            .map(|claim| Claim {
                holder: self.outward(&claim.holder),
                ..claim
            })
            .collect()
    }
}
