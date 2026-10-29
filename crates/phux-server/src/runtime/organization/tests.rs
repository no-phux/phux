use phux_protocol::ids::{OperationId, Revision};
use phux_protocol::wire::frame::{
    ArrangementDoc, Command, CommandResult, CommandValue, Committed, ExecState, LaunchSpec,
    LiveTarget, OrgFilter, OrganizationPage, Refusal as WireRefusal, ResourceRecord,
    SessionLifetime, SessionRecord, TerminateMode,
};
use tokio_util::sync::CancellationToken;

use crate::org_wire as ow;
use crate::state::{ClientId, ServerState, SharedState};

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

mod arrangements;
mod create;
mod paging;
mod session;
