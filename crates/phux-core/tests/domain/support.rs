//! A registry harness that runs commands with deterministic keys and checks
//! every organization invariant after each commit.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    dead_code,
    unreachable_pub,
    reason = "shared test support"
)]

use phux_core::{
    Actor, Applied, AuthorityId, Command, ExitPolicy, InstanceId, LaunchSpec, NewResource, Op,
    OperationId, Org, Refusal, Registry, Resource, ResourceKey, ResourceKind, SequentialKeys,
    Session, SessionKey, SessionLifetime, Stamp, TerminalLaunch,
};

pub const INSTANCE: InstanceId = InstanceId::from_bytes([9; 16]);

pub struct Harness {
    pub reg: Registry,
    pub keys: SequentialKeys,
    pub ops: u64,
    pub now: u64,
}

impl Harness {
    pub fn new() -> Self {
        let org = Org::new(
            AuthorityId::from_bytes([1; 16]),
            SessionKey::from_bytes([2; 16]),
            INSTANCE,
            Stamp(1),
        );
        Self {
            reg: Registry::new(org),
            keys: SequentialKeys::new(*b"testkeys"),
            ops: 0,
            now: 1,
        }
    }

    pub const fn inbox(&self) -> SessionKey {
        self.reg.org().authority().inbox_session
    }

    pub fn next_op(&mut self) -> OperationId {
        self.ops += 1;
        let mut bytes = [0xaa; 16];
        bytes[8..].copy_from_slice(&self.ops.to_be_bytes());
        OperationId::from_bytes(bytes)
    }

    /// Run `op` as `actor` with a fresh operation id.
    pub fn run_as(&mut self, actor: &Actor, op: Op) -> Result<Applied, Refusal> {
        let operation = self.next_op();
        self.run_cmd(actor, &Command { operation, op })
    }

    /// Run `cmd` as `actor`; on success commit it and check invariants.
    pub fn run_cmd(&mut self, actor: &Actor, cmd: &Command) -> Result<Applied, Refusal> {
        self.now += 1;
        let before = self.reg.org().clone();
        let result = self
            .reg
            .execute(cmd, actor, Stamp(self.now), &mut self.keys)
            .map(|(applied, _)| applied);
        match &result {
            Ok(_) => {
                if let Err(e) = self.reg.org().check_invariants() {
                    panic!("{e} after {:?}", cmd.op);
                }
            }
            Err(_) => assert_eq!(self.reg.org(), &before, "a refusal changed state"),
        }
        result
    }

    pub fn run(&mut self, op: Op) -> Result<Applied, Refusal> {
        self.run_as(&Actor::owner(), op)
    }

    pub fn report(&mut self, op: Op) -> Result<Applied, Refusal> {
        self.run_as(&Actor::system(), op)
    }

    pub fn session(&self, key: SessionKey) -> Session {
        self.reg
            .org()
            .session(&key)
            .cloned()
            .expect("session exists")
    }

    pub fn resource(&self, key: ResourceKey) -> Resource {
        self.reg
            .org()
            .resource(&key)
            .cloned()
            .expect("resource exists")
    }

    pub fn create_session(&mut self, name: &str, lifetime: SessionLifetime) -> SessionKey {
        let resource = (lifetime == SessionLifetime::Ephemeral).then(shell);
        let applied = self
            .run(Op::SessionCreate {
                name: name.to_owned(),
                lifetime,
                attrs: phux_core::Attrs::default(),
                resource,
            })
            .expect("session create");
        created(&applied).0.expect("session key")
    }

    pub fn create_resource(&mut self, session: SessionKey, spec: NewResource) -> ResourceKey {
        let applied = self
            .run(Op::ResourceCreate {
                placement: None,
                session: Some(session),
                resource: spec,
            })
            .expect("resource create");
        created(&applied).1.expect("resource key")
    }

    pub fn shell_in(&mut self, session: SessionKey) -> ResourceKey {
        self.create_resource(session, shell())
    }
}

pub const fn created(applied: &Applied) -> (Option<SessionKey>, Option<ResourceKey>) {
    match applied.reply {
        phux_core::Reply::Ok { created, .. } | phux_core::Reply::Accepted { created } => {
            (created.session, created.resource)
        }
    }
}

/// An interactive shell: removed when it exits.
pub fn shell() -> NewResource {
    NewResource {
        kind: ResourceKind::Terminal,
        name: None,
        launch: Some(LaunchSpec::Terminal(TerminalLaunch::default())),
        on_exit: ExitPolicy::Remove,
        parent: None,
        launch_mode: phux_core::LaunchMode::Immediate,
    }
}

/// A launched command: retained with its outcome.
pub fn job() -> NewResource {
    NewResource {
        on_exit: ExitPolicy::Retain,
        ..shell()
    }
}
