//! Delegated agent credentials through the closed scope matrix (ADR-0163
//! §5, R22): an agent holds `INVENTORY`, `OBSERVE`, and `INPUT` on its own
//! session and nothing else, and a delegation never widens what its
//! launcher holds.

use std::time::Duration;

use chrono::Utc;
use phux_core::PrincipalId;
use phux_protocol::kinds::{Requirement, Verb, Verbs, frame_rule};
use phux_protocol::wire::frame::{Command, FrameKind, TerminalSignal};

use super::{
    ConnectionGrant, World, admits, admits_command, command, get_screen, organization_samples,
    samples, scoped, world,
};
use crate::delegation::{AGENT_VERBS, DelegatedCredential, DelegationError, DelegationRequest};
use crate::state::ClientId;

/// The connection that launches the agent.
const LAUNCHER: ClientId = ClientId(50);

/// Mint, as `launcher_grant`, a delegation on `alpha`.
fn delegate(
    world: &mut World,
    launcher_grant: ConnectionGrant,
    request: DelegationRequest,
) -> Result<DelegatedCredential, DelegationError> {
    world.state.set_connection_grant(LAUNCHER, launcher_grant);
    world
        .state
        .mint_delegation(LAUNCHER, world.alpha_session, request, Utc::now())
}

/// The grant an agent the owner launched on `alpha` connects with.
fn agent(world: &mut World) -> ConnectionGrant {
    let credential = delegate(
        world,
        ConnectionGrant::owner(),
        DelegationRequest::default(),
    )
    .expect("the owner may delegate the agent default");
    world
        .state
        .delegated_grant(&credential.id, Utc::now())
        .expect("a fresh credential admits")
}

fn request(verbs: &[Verb]) -> DelegationRequest {
    DelegationRequest {
        verbs: Verbs::of(verbs),
        ..DelegationRequest::default()
    }
}

#[test]
fn a_delegated_agent_is_a_credential_acting_for_the_owner() {
    let mut world = world();
    let credential = delegate(
        &mut world,
        ConnectionGrant::owner(),
        DelegationRequest::default(),
    )
    .unwrap();
    assert_eq!(credential.verbs, AGENT_VERBS);
    assert_eq!(credential.on_behalf_of, PrincipalId::Owner);
    let grant = world
        .state
        .delegated_grant(&credential.id, Utc::now())
        .unwrap();
    assert_eq!(grant.principal(), &credential.principal());
    assert!(!grant.principal().is_human());
    assert_eq!(grant.on_behalf_of(), Some(&PrincipalId::Owner));
    assert!(grant.expires_at.is_some_and(|at| at > Utc::now()));
}

/// Every row the agent's grant admits needs only agent verbs; in
/// particular no `SIGNAL`, `TERMINATE`, `ORGANIZE`, `CREATE`, or `BIND`.
#[test]
fn a_delegated_agent_is_admitted_only_rows_needing_agent_verbs() {
    let mut world = world();
    let grant = agent(&mut world);
    let mut admitted = 0;
    for frame in &samples(&world) {
        if !admits(&world, &grant, frame) {
            continue;
        }
        admitted += 1;
        if let Requirement::Verbs(verbs) = frame_rule(frame).requirement {
            assert_eq!(
                verbs.bits() & !AGENT_VERBS.bits(),
                0,
                "`{}` admitted with {verbs:?}: {frame:?}",
                frame_rule(frame).case
            );
        }
    }
    assert!(admitted > 0, "the agent can still observe and type");
}

#[test]
fn a_delegated_agent_is_refused_terminate_signal_and_organize_even_in_its_session() {
    let mut world = world();
    let grant = agent(&mut world);
    let alpha = world.alpha.clone();
    for refused in [
        Command::KillResource {
            terminal_id: alpha.clone(),
            operation_id: None,
        },
        Command::SignalTerminal {
            terminal_id: alpha.clone(),
            signal: TerminalSignal::Interrupt,
            operation_id: None,
        },
    ] {
        assert!(
            !admits_command(&world, &grant, refused.clone()),
            "{refused:?}"
        );
    }
    for frame in organization_samples(&world, &alpha) {
        let FrameKind::Command { command: org, .. } = &frame else {
            unreachable!("organization samples are commands");
        };
        let reads = matches!(
            org,
            Command::GetOrganization { .. } | Command::ArrangementGet { .. }
        );
        if !reads {
            assert!(!admits(&world, &grant, &frame), "{org:?}");
        }
    }
}

#[test]
fn a_delegated_agent_is_confined_to_its_own_session() {
    let mut world = world();
    let grant = agent(&mut world);
    assert!(admits_command(
        &world,
        &grant,
        get_screen(world.alpha.clone())
    ));
    assert!(!admits_command(
        &world,
        &grant,
        get_screen(world.beta.clone())
    ));
    let key = |terminal_id| {
        command(Command::RouteInput {
            terminal_id,
            event: phux_protocol::input::InputEvent::Focus(
                phux_protocol::input::focus::FocusEvent::Gained,
            ),
        })
    };
    assert!(admits(&world, &grant, &key(world.alpha.clone())));
    assert!(!admits(&world, &grant, &key(world.beta.clone())));
}

#[test]
fn delegation_refuses_every_verb_outside_the_agent_set() {
    for verb in [
        Verb::Create,
        Verb::Bind,
        Verb::Signal,
        Verb::Organize,
        Verb::Terminate,
    ] {
        let mut world = world();
        assert_eq!(
            delegate(
                &mut world,
                ConnectionGrant::owner(),
                request(&[Verb::Observe, verb])
            ),
            Err(DelegationError::Widens(verb)),
            "{verb:?}"
        );
    }
}

#[test]
fn delegation_never_widens_the_launchers_grant() {
    let mut world = world();
    let g = world.alpha_group;
    // The launcher may observe its session but not type into it.
    let observer = scoped(&[&format!("inventory,observe@group:{g}")]);
    assert_eq!(
        delegate(&mut world, observer.clone(), DelegationRequest::default()),
        Err(DelegationError::LauncherLacks(Verb::Input))
    );
    // Narrowing to what it holds succeeds.
    let narrowed = delegate(&mut world, observer, request(&[Verb::Observe])).unwrap();
    assert_eq!(narrowed.verbs, Verbs::of(&[Verb::Observe]));

    // Every verb on another session is not every verb on this one.
    let elsewhere = scoped(&[&format!(
        "inventory,observe,input@group:{}",
        world.state.idspace.intern_session(world.beta_session).get()
    )]);
    assert_eq!(
        delegate(&mut world, elsewhere, DelegationRequest::default()),
        Err(DelegationError::LauncherLacks(Verb::Inventory))
    );
    // One Terminal of the session is not the whole session.
    let one_terminal = scoped(&[&format!(
        "inventory,observe,input@terminal:{}",
        super::local_id(&world.alpha)
    )]);
    assert_eq!(
        delegate(&mut world, one_terminal, DelegationRequest::default()),
        Err(DelegationError::LauncherLacks(Verb::Inventory))
    );
}

#[test]
fn a_delegate_redelegating_keeps_its_human_and_cannot_outlive_itself() {
    let mut world = world();
    let parent = agent(&mut world);
    let parent_expiry = parent.expires_at.unwrap();
    let child = delegate(
        &mut world,
        parent.clone(),
        DelegationRequest {
            verbs: Verbs::of(&[Verb::Observe]),
            lifetime: Duration::from_hours(30 * 24),
        },
    )
    .unwrap();
    assert_eq!(child.on_behalf_of, PrincipalId::Owner);
    assert_eq!(&child.delegator, parent.principal());
    assert!(child.expires_at <= parent_expiry);
}

#[test]
fn a_lifetime_is_clamped_and_a_lapsed_launcher_mints_nothing() {
    let mut world = world();
    let long = delegate(
        &mut world,
        ConnectionGrant::owner(),
        DelegationRequest {
            lifetime: Duration::from_hours(365 * 24),
            ..DelegationRequest::default()
        },
    )
    .unwrap();
    assert!(
        long.expires_at
            <= Utc::now() + chrono::Duration::from_std(crate::delegation::MAX_LIFETIME).unwrap()
    );
    let mut lapsed = scoped(&["*@global"]);
    lapsed.expires_at = Some(Utc::now() - chrono::Duration::seconds(1));
    assert_eq!(
        delegate(&mut world, lapsed, DelegationRequest::default()),
        Err(DelegationError::NoLauncherGrant)
    );
}

#[test]
fn a_withdrawn_or_expired_credential_admits_nothing() {
    let mut world = world();
    let credential = delegate(
        &mut world,
        ConnectionGrant::owner(),
        DelegationRequest::default(),
    )
    .unwrap();
    let later = credential.expires_at + chrono::Duration::seconds(1);
    assert!(world.state.delegated_grant(&credential.id, later).is_none());

    world.state.set_connection_grant(
        ClientId(60),
        world
            .state
            .delegated_grant(&credential.id, Utc::now())
            .unwrap(),
    );
    assert_eq!(
        world.state.withdraw_delegation(&credential.id),
        vec![ClientId(60)],
        "withdrawal names the live connections to revoke"
    );
    assert!(
        world
            .state
            .delegated_grant(&credential.id, Utc::now())
            .is_none()
    );
}
