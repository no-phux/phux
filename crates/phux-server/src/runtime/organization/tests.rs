use phux_protocol::ids::{OperationId, Revision};
use phux_protocol::wire::frame::{
    ArrangementDoc, ExecState, LaunchSpec, Refusal as WireRefusal, SessionLifetime, TerminateMode,
};

use super::*;

const CLIENT: ClientId = ClientId(3);

fn op(n: u8) -> OperationId {
    OperationId::new([n; 16]).unwrap()
}

fn committed(result: CommandResult) -> Committed {
    match result {
        CommandResult::OkWith(CommandValue::Committed(c)) => *c,
        other => panic!("expected a committed receipt, got {other:?}"),
    }
}

fn page(state: &SharedState, filter: OrgFilter) -> OrganizationPage {
    let result = handle(
        state,
        CLIENT,
        Command::GetOrganization {
            filter,
            page_size: 0,
            cursor: None,
        },
        &CancellationToken::new(),
    );
    match result {
        CommandResult::OkWith(CommandValue::OrganizationPage(p)) => *p,
        other => panic!("expected a page, got {other:?}"),
    }
}

fn run<F: std::future::Future<Output = ()>>(f: F) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&rt, f);
}

/// Pages of one organization, with a fresh operation id per mutation.
struct Org {
    state: SharedState,
    next_op: u8,
}

impl Org {
    fn new() -> Self {
        Self {
            state: SharedState::new(),
            next_op: 1,
        }
    }

    fn operation(&mut self) -> OperationId {
        let n = self.next_op;
        self.next_op = self.next_op.checked_add(1).expect("operation ids");
        op(n)
    }

    fn create_session(&mut self, name: &str) -> phux_protocol::ids::SessionKey {
        let operation = self.operation();
        committed(handle(
            &self.state,
            CLIENT,
            Command::SessionCreate {
                operation,
                name: name.to_owned(),
                lifetime: SessionLifetime::Persistent,
                attrs: Vec::new(),
                launch: None,
            },
            &CancellationToken::new(),
        ))
        .session
        .unwrap_or_else(|| panic!("created {name}"))
    }

    fn create_resource(
        &mut self,
        session: phux_protocol::ids::SessionKey,
    ) -> phux_protocol::ids::ResourceKey {
        use phux_protocol::wire::frame::{ExitPolicy, LaunchMode};
        let operation = self.operation();
        committed(handle(
            &self.state,
            CLIENT,
            Command::ResourceCreate {
                placement: None,
                operation,
                session,
                kind: phux_protocol::ids::ResourceKind::Terminal,
                launch: Some(Box::new(LaunchSpec::new(vec!["sh".to_owned()]))),
                name: None,
                on_exit: ExitPolicy::Retain,
                launch_mode: LaunchMode::Deferred,
            },
            &CancellationToken::new(),
        ))
        .resource
        .expect("created resource")
    }

    fn delete_session(&mut self, session: &SessionRecord) {
        let operation = self.operation();
        let result = handle(
            &self.state,
            CLIENT,
            Command::SessionDelete {
                operation,
                session: session.key,
            },
            &CancellationToken::new(),
        );
        assert!(
            matches!(result, CommandResult::OkWith(_)),
            "delete {}: {result:?}",
            session.name
        );
    }

    fn remove_resource(&mut self, resource: &ResourceRecord) {
        let operation = self.operation();
        let result = handle(
            &self.state,
            CLIENT,
            Command::ResourceRemove {
                operation,
                resource: resource.key,
            },
            &CancellationToken::new(),
        );
        assert!(
            matches!(result, CommandResult::OkWith(_)),
            "remove {}: {result:?}",
            resource.key
        );
    }

    fn list(&self, filter: OrgFilter, page_size: u32, cursor: Option<Vec<u8>>) -> OrganizationPage {
        match handle(
            &self.state,
            CLIENT,
            Command::GetOrganization {
                filter,
                page_size,
                cursor,
            },
            &CancellationToken::new(),
        ) {
            CommandResult::OkWith(CommandValue::OrganizationPage(page)) => *page,
            other => panic!("expected a page, got {other:?}"),
        }
    }

    fn drain(
        &self,
        filter: OrgFilter,
        page_size: u32,
        mut cursor: Option<Vec<u8>>,
    ) -> (Vec<SessionRecord>, Vec<ResourceRecord>) {
        let mut sessions = Vec::new();
        let mut resources = Vec::new();
        for _ in 0..32 {
            let Some(token) = cursor else {
                return (sessions, resources);
            };
            let page = self.list(filter, page_size, Some(token));
            sessions.extend(page.sessions);
            resources.extend(page.resources);
            cursor = page.next_cursor;
        }
        panic!("paging did not finish");
    }
}

/// The greatest 16-byte key strictly below `cursor` that names nothing yet.
fn free_key_before(state: &SharedState, cursor: &[u8; 16]) -> [u8; 16] {
    let mut bytes = *cursor;
    loop {
        let mut stepped = false;
        for byte in bytes.iter_mut().rev() {
            if *byte == 0 {
                *byte = 0xff;
                continue;
            }
            *byte -= 1;
            stepped = true;
            break;
        }
        assert!(stepped && bytes != [0; 16], "no free key below the cursor");
        let taken = state.with(|s| {
            let org = s.org();
            org.session(&phux_core::SessionKey::from_bytes(bytes))
                .is_some()
                || org
                    .resource(&phux_core::ResourceKey::from_bytes(bytes))
                    .is_some()
                || org
                    .arrangement(&phux_core::ArrangementKey::from_bytes(bytes))
                    .is_some()
        });
        if !taken {
            return bytes;
        }
    }
}

fn insert_session_before(
    state: &SharedState,
    cursor: phux_protocol::ids::SessionKey,
) -> phux_protocol::ids::SessionKey {
    let bytes = free_key_before(state, cursor.as_bytes());
    let key = phux_core::SessionKey::from_bytes(bytes);
    let session = phux_core::Session {
        key,
        name: "inserted-before".to_owned(),
        lifetime: phux_core::SessionLifetime::Persistent,
        awaiting_organization: false,
        members: Vec::new(),
        arrangement: None,
        attrs: phux_core::Attrs::default(),
        created: phux_core::Stamp(0),
        revision: phux_core::Revision::FIRST,
        recent: None,
    };
    state.with_mut(|s| {
        s.registry_mut().commit(&phux_core::Transition {
            deltas: vec![phux_core::Delta::PutSession(session)],
            ..phux_core::Transition::default()
        });
    });
    phux_protocol::ids::SessionKey::new(bytes).expect("non-zero key")
}

fn insert_resource_before(
    state: &SharedState,
    session: phux_protocol::ids::SessionKey,
    cursor: phux_protocol::ids::ResourceKey,
) -> phux_protocol::ids::ResourceKey {
    let bytes = free_key_before(state, cursor.as_bytes());
    let key = phux_core::ResourceKey::from_bytes(bytes);
    let session_key = phux_core::SessionKey::from_bytes(*session.as_bytes());
    state.with_mut(|s| {
        let mut record = s.org().session(&session_key).expect("session").clone();
        record.members.push(key);
        let resource = phux_core::Resource {
            key,
            session: session_key,
            name: None,
            kind: phux_core::ResourceKind::Terminal,
            provenance: phux_core::Provenance {
                created_by: phux_core::Actor::system(),
                operation: phux_core::OperationId::from_bytes([9; 16]),
                parent: None,
            },
            launch: None,
            on_exit: phux_core::ExitPolicy::Retain,
            durable_result: false,
            activation: None,
            revision: phux_core::Revision::FIRST,
        };
        s.registry_mut().commit(&phux_core::Transition {
            deltas: vec![
                phux_core::Delta::PutResource(resource),
                phux_core::Delta::PutSession(record),
            ],
            ..phux_core::Transition::default()
        });
    });
    phux_protocol::ids::ResourceKey::new(bytes).expect("non-zero key")
}

mod authority;
mod evidence;
mod mutations;
mod paging;
