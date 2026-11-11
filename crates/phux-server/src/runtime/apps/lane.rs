//! The server's input lane (ADR-0044) as the host carries it. The app that
//! runs one starts it ([`super::start`]); the host hands a clone
//! to every connection, asks it which commands ride it, and lends it back to
//! the apps with the connection's own state on it. What travels on the lane,
//! and how, is that app's.

use std::any::Any;
use std::sync::Arc;

use futures_util::future::LocalBoxFuture;
use phux_protocol::wire::frame::{Command, CommandResult};

use crate::state::{ClientId, SharedState};

/// What the host asks of a running lane. The app that runs it reads its
/// own handle back through [`InputLane::get`].
pub(crate) trait Lane: Any + Send + Sync + std::fmt::Debug {
    /// Whether `command` rides the lane rather than the command handler.
    fn rides(&self, command: &Command) -> bool;

    /// A new connection's state on the lane, kept with it for its lifetime.
    fn connect(&self) -> Box<dyn Any>;

    /// Begin a `command` that [`Self::rides`] the lane. `Err` is its result
    /// now; `Ok` resolves to it once the lane has carried it.
    fn begin<'a>(
        &'a self,
        on: LaneCommand<'a>,
        command: Command,
    ) -> LocalBoxFuture<'a, Result<LocalBoxFuture<'static, CommandResult>, CommandResult>>;
}

/// The connection a command begins on the lane for.
pub(crate) struct LaneCommand<'a> {
    pub(crate) state: &'a SharedState,
    pub(crate) client_id: ClientId,
    /// The connection's state on the lane, from [`Lane::connect`].
    pub(crate) connection: &'a mut (dyn Any + 'static),
}

/// A cloneable handle on the running lane. Every clone keeps it running.
#[derive(Clone, Debug)]
pub struct InputLane(Arc<dyn Lane>);

impl InputLane {
    pub(crate) fn new(lane: impl Lane) -> Self {
        Self(Arc::new(lane))
    }

    /// The app's own handle, when the lane is a `T`.
    pub(crate) fn get<T: Lane>(&self) -> Option<&T> {
        let lane: &dyn Any = self.0.as_ref();
        lane.downcast_ref()
    }

    /// Whether `command` rides the lane.
    pub(crate) fn rides(&self, command: &Command) -> bool {
        self.0.rides(command)
    }

    /// One connection's place on the lane.
    pub(crate) fn connect(&self) -> ConnectionLane {
        ConnectionLane {
            lane: self.clone(),
            connection: self.0.connect(),
        }
    }
}

/// One connection's place on the lane: the lane, and the state its app keeps
/// for that connection.
#[derive(Debug)]
pub(crate) struct ConnectionLane {
    lane: InputLane,
    connection: Box<dyn Any>,
}

impl ConnectionLane {
    /// The lane this connection is on.
    pub(crate) const fn lane(&self) -> &InputLane {
        &self.lane
    }

    /// The app's handle and this connection's state, when they are an `L`
    /// and an `S`.
    pub(crate) fn parts<L: Lane, S: Any>(&mut self) -> Option<(&L, &mut S)> {
        Some((self.lane.get()?, self.connection.downcast_mut()?))
    }

    /// Begin `command`, which rides the lane, as `client_id`.
    pub(crate) fn begin<'a>(
        &'a mut self,
        state: &'a SharedState,
        client_id: ClientId,
        command: Command,
    ) -> LocalBoxFuture<'a, Result<LocalBoxFuture<'static, CommandResult>, CommandResult>> {
        let on = LaneCommand {
            state,
            client_id,
            connection: self.connection.as_mut(),
        };
        self.lane.0.begin(on, command)
    }
}

/// A started lane. Dropping it stops the lane once every [`InputLane`] clone
/// is gone, so drop every task that may hold one first.
pub(crate) struct RunningLane {
    /// Declared first, so it is dropped before `thread` waits on it.
    handle: InputLane,
    #[allow(dead_code, reason = "held for its Drop, which stops the lane")]
    thread: Box<dyn Any>,
}

impl RunningLane {
    /// `handle` on a lane `thread` owns; dropping `thread` stops it.
    pub(crate) fn new(handle: InputLane, thread: impl Any) -> Self {
        Self {
            handle,
            thread: Box::new(thread),
        }
    }

    /// A handle for the connections.
    pub(crate) fn handle(&self) -> InputLane {
        self.handle.clone()
    }
}

impl std::fmt::Debug for RunningLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningLane")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}
