//! Ordered CLI wire regressions: no legacy attach policy or fresh-operation retry.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use bytes::BytesMut;
use phux_client::attach::Dial;
use phux_protocol::caps::{BootstrapCapabilities, ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet, select_bootstrap_profile};
use phux_protocol::ids::{AuthorityId, ResourceId, ResourceKey, Revision, SessionId, SessionKey};
use phux_protocol::wire::frame::{AttachTarget, Command, CommandResult, CommandValue, Committed, FrameKind, OrgFilter, OrganizationPage, Refusal, SessionLifetime, SessionRecord};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::commands::server_target::ServerTarget;

#[derive(Clone, Copy)]
pub(crate) enum Reply {
    Created,
    Existing,
    Collision,
    Lost,
    Unknown,
    MissingIdentity,
    MissingEnsureOutcome,
    Legacy,
    NoEnsure,
    ReusedName,
    Removed,
}

pub(crate) struct Fixture {
    _root: tempfile::TempDir,
    pub(crate) socket: PathBuf,
    peer: tokio::task::JoinHandle<Vec<Command>>,
}

impl Fixture {
    pub(crate) fn start(reply: Reply) -> Self {
        let root = tempfile::tempdir().expect("fixture directory");
        let socket = root.path().join("session.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("fixture listener");
        let peer = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), async {
                let (mut stream, _) = listener.accept().await.expect("CLI connects");
                let FrameKind::Hello { client_caps, .. } = recv(&mut stream).await.expect("HELLO") else {
                    panic!("first frame must negotiate");
                };
                let (selected_profile, bootstrap_limits) = select_bootstrap_profile(&client_caps, &BootstrapCapabilities::new()).expect("compatible profile");
                let features = match reply {
                    Reply::Legacy => ServerFeatureExtSet::with(&[]),
                    Reply::NoEnsure => ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]),
                    _ => ServerFeatureExtSet::with(&[ServerFeatureExt::Organization, ServerFeatureExt::SessionEnsure]),
                };
                send(&mut stream, FrameKind::HelloOk {
                    protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                    protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                    protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
                    server_caps: ServerCapabilities::new().with_features_ext(features),
                    server_id: Vec::new(), selected_profile: Some(selected_profile), bootstrap_limits,
                }).await;
                let key = SessionKey::new([0x41; 16]).unwrap();
                let rev = Revision::new(1);
                let mut commands = Vec::new();
                while let Some(frame) = recv(&mut stream).await {
                    let FrameKind::Command { request_id, command } = frame else {
                        panic!("CLI must not send legacy attach/spawn: {frame:?}");
                    };
                    let result = match &command {
                        Command::SessionCreate { operation, name, .. }
                        | Command::SessionEnsure { operation, name, .. } => {
                            assert_eq!(name, "work");
                            assert!(commands.is_empty(), "no second mutation or preflight race");
                            if matches!(reply, Reply::Lost) {
                                commands.push(command);
                                drop(stream);
                                assert!(tokio::time::timeout(Duration::from_millis(100), listener.accept()).await.is_err(), "lost receipt must not reconnect with a fresh create");
                                return commands;
                            }
                            match reply {
                                Reply::Legacy | Reply::NoEnsure => panic!("unsupported peers must accept no mutation"),
                                Reply::Collision => CommandResult::Refused(Refusal::NameTaken),
                                Reply::Unknown => CommandResult::Ok,
                                _ => {
                                    let mut receipt = Committed::new(*operation, rev);
                                    if !matches!(reply, Reply::MissingIdentity) { receipt.session = Some(key); }
                                    if matches!(command, Command::SessionEnsure { .. }) && !matches!(reply, Reply::MissingEnsureOutcome) {
                                        receipt.ensured_created = Some(!matches!(reply, Reply::Existing));
                                    }
                                    CommandResult::OkWith(CommandValue::Committed(Box::new(receipt)))
                                }
                            }
                        }
                        Command::GetOrganization { filter, .. } => {
                            assert_eq!(*filter, OrgFilter::Session(key), "selection must use receipt key, never reusable name");
                            let mut sessions = if matches!(reply, Reply::Removed) { Vec::new() } else {
                                vec![SessionRecord::new(key, "renamed-after-create", rev).with_live(Some(SessionId::new(7))) ]
                            };
                            if matches!(reply, Reply::ReusedName | Reply::Removed) {
                                sessions.insert(0, SessionRecord::new(SessionKey::new([0x42; 16]).unwrap(), "work", rev).with_live(Some(SessionId::new(99))));
                            }
                            let mut resources = if matches!(reply, Reply::Removed) { Vec::new() } else {
                                vec![
                                    phux_protocol::wire::frame::ResourceRecord::new(ResourceKey::new([0x51; 16]).unwrap(), key, rev).with_live(Some(ResourceId::local(4))),
                                    phux_protocol::wire::frame::ResourceRecord::new(ResourceKey::new([0x52; 16]).unwrap(), key, rev).with_live(Some(ResourceId::local(9))),
                                ]
                            };
                            if matches!(reply, Reply::ReusedName | Reply::Removed) {
                                resources.insert(0, phux_protocol::wire::frame::ResourceRecord::new(
                                    ResourceKey::new([0x53; 16]).unwrap(), SessionKey::new([0x42; 16]).unwrap(), rev,
                                ).with_live(Some(ResourceId::local(1))));
                            }
                            CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(OrganizationPage::new(AuthorityId::new([0x43; 16]).unwrap(), rev, 0).with_sessions(sessions).with_resources(resources))))
                        }
                        _ => panic!("unexpected CLI operation: {command:?}"),
                    };
                    commands.push(command);
                    send(&mut stream, FrameKind::CommandResult { request_id, result }).await;
                }
                assert!(tokio::time::timeout(Duration::from_millis(100), listener.accept()).await.is_err(), "workflow must not retry on a new connection");
                commands
            }).await.expect("bounded CLI exchange")
        });
        Self { _root: root, socket, peer }
    }

    pub(crate) fn dial(&self) -> Dial { Dial::uds(&self.socket) }
    fn target(&self) -> ServerTarget { ServerTarget::Local(self.socket.clone()) }
    pub(crate) async fn finish(self) -> Vec<Command> { self.peer.await.expect("fixture completed") }
}

async fn recv(stream: &mut tokio::net::UnixStream) -> Option<FrameKind> {
    let mut header = [0; 4];
    match stream.read_exact(&mut header).await {
        Ok(_) => {},
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return None,
        Err(err) => panic!("read frame: {err}"),
    }
    let mut body = vec![0; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut body).await.expect("frame body");
    let mut bytes = header.to_vec();
    bytes.extend(body);
    let (frame, tail) = FrameKind::decode(&bytes).expect("valid frame");
    assert!(tail.is_empty());
    Some(frame)
}

async fn send(stream: &mut tokio::net::UnixStream, frame: FrameKind) {
    let mut bytes = BytesMut::new();
    frame.encode(&mut bytes);
    stream.write_all(&bytes).await.expect("fixture reply");
}

#[tokio::test]
async fn interactive_new_is_exact_for_both_lifetimes_and_selects_the_receipt_key() {
    for lifetime in [SessionLifetime::Persistent, SessionLifetime::Ephemeral] {
        let fixture = Fixture::start(Reply::ReusedName);
        let result = super::create::create_attached_session(&fixture.target(), "work", vec!["agent".into()], Some("/work".into()), lifetime).await;
        assert_eq!(result.unwrap(), AttachTarget::ById(SessionId::new(7)));
        let commands = fixture.finish().await;
        assert_eq!(commands.len(), 2);
        assert!(matches!(&commands[0], Command::SessionCreate { lifetime: actual, launch: Some(launch), .. } if *actual == lifetime && launch.command == ["agent"] && launch.cwd.as_deref() == Some("/work")));
    }
}

#[tokio::test]
async fn interactive_new_collision_unknown_and_lost_receipts_never_ensure_or_retry() {
    for lifetime in [SessionLifetime::Persistent, SessionLifetime::Ephemeral] {
        for reply in [Reply::Collision, Reply::Unknown, Reply::Lost, Reply::MissingIdentity, Reply::Removed, Reply::Legacy] {
            let fixture = Fixture::start(reply);
            let result = super::create::create_attached_session(&fixture.target(), "work", Vec::new(), None, lifetime).await;
            assert!(result.is_err());
            let commands = fixture.finish().await;
            assert_eq!(commands.iter().filter(|command| matches!(command, Command::SessionCreate { .. })).count(), usize::from(!matches!(reply, Reply::Legacy)));
            assert!(!commands.iter().any(|command| matches!(command, Command::SessionEnsure { .. })));
        }
    }
}

#[tokio::test]
async fn headless_new_does_not_turn_collision_or_unknown_receipt_into_success() {
    for reply in [Reply::Collision, Reply::Unknown, Reply::Lost, Reply::Legacy] {
        let fixture = Fixture::start(reply);
        let result = super::create::create_seeded_session(&fixture.target(), "work", None, None, BTreeMap::new(), None, true, None, SessionLifetime::Persistent, &[]).await;
        assert_eq!(result.unwrap_err(), ExitCode::FAILURE);
        let commands = fixture.finish().await;
        assert_eq!(commands.len(), usize::from(!matches!(reply, Reply::Legacy)));
    }
}

#[tokio::test]
async fn empty_headless_new_reports_committed_creation_without_an_attach_readback() {
    let fixture = Fixture::start(Reply::Created);
    let receipt = super::empty::create_empty_session_via_metadata(&fixture.target(), "work", true).await.unwrap();
    assert!(receipt.session.is_some());
    let commands = fixture.finish().await;
    assert_eq!(commands.len(), 1);
    assert!(matches!(&commands[0], Command::SessionCreate { launch: None, lifetime: SessionLifetime::Persistent, .. }));
}

#[test]
fn committed_no_live_creation_preserves_applied_recovery_and_exact_identities() {
    let operation = phux_client::org::mint_operation();
    let session = SessionKey::new([0x71; 16]).unwrap();
    let resource = ResourceKey::new([0x72; 16]).unwrap();
    let authority = AuthorityId::new([0x73; 16]).unwrap();
    let receipt = Committed::new(operation, Revision::new(1))
        .with_session(Some(session)).with_resource(Some(resource))
        .with_creation(Some(authority), None, None, None);
    let error = super::committed_session_error("work", &receipt);
    let doc = phux_client::contract::error_document(&error, 1);
    assert_eq!(doc["error"]["code"], "session_create_failed");
    assert_eq!(doc["error"]["recovery"]["effect_certainty"], "applied");
    assert_eq!(doc["error"]["recovery"]["next_action"], "reconcile");
    assert_eq!(doc["error"]["recovery"]["operation_id"], operation.to_hex());
    assert_eq!(doc["error"]["recovery"]["scope"]["id"], authority.to_string());
    assert!(error.message.contains(&session.to_string()));
    assert!(error.message.contains(&resource.to_string()));
}

#[tokio::test]
async fn headless_committed_without_live_target_is_not_success_or_unavailable() {
    let fixture = Fixture::start(Reply::Created);
    let result = super::create::create_seeded_session(
        &fixture.target(), "work", None, None, BTreeMap::new(), None, true,
        None, SessionLifetime::Persistent, &[],
    ).await;
    assert_eq!(result.unwrap_err(), ExitCode::FAILURE);
    let commands = fixture.finish().await;
    assert_eq!(commands.len(), 1, "a committed launch intent cannot authorize another create");
    assert!(matches!(&commands[0], Command::SessionCreate { .. }));
}
