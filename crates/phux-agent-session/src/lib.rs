//! Agent sessions, over the kind-neutral [`phux_resource`] substrate.
//!
//! [`session`] is the `AgentSession` engine: a producer-fed record stream.
//! [`state`], [`asked`], and [`agent_record`] decide what the `phux.agent/v1`
//! record says and who may say it, whichever kind the evidence came from.
//! [`parent`] is how a session and those reports reach the resource an agent
//! runs in, so the host need not name that resource's engine.

pub mod agent_record;
pub mod asked;
pub mod parent;
pub mod session;
pub mod state;
