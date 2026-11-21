//! Native agent view publication from decoded stream frames.

use super::*;

#[test]
fn engine_accessor_exposes_only_committed_views_and_disconnects_closed_sources() {
    use phux_protocol::native_agent::{NativeCapabilities, NativeView};
    #[cfg(feature = "engine")]
    let (engine, _publication) = owner();
    #[cfg(not(feature = "engine"))]
    let engine = owner();
    let agent = id(22);
    let stream_id = stream(3);
    let bootstrap_id = bootstrap(4);
    apply_ok(
        &engine,
        EngineEvent::AgentSessionDeclared {
            terminal_id: agent.clone(),
            parent: Some(id(7)),
            provider: Some("omp".into()),
            native_id: None,
            state: Some("working".into()),
        },
    );
    assert_eq!(engine.native_agent_view(&agent), None);
    let view = NativeView {
        activation: "activation-A".into(),
        backend: "acp".into(),
        capabilities: NativeCapabilities {
            prompt: true.into(),
            ..NativeCapabilities::default()
        },
        connected: true,
        turn_id: None,
        provider_session_id: None,
        questions: vec![],
    };
    let data =
        serde_json::json!({"schema":"phux.native-agent/v1","update":{"type":"view","view":view}});
    let payload = format!("{{\"seq\":1,\"ts_ms\":1,\"type\":\"provider_raw\",\"data\":{data}}}\n");
    apply_ok(
        &engine,
        EngineEvent::Stream(FrameKind::BootstrapBegin {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::AgentEventsJsonlV1,
            cols: 0,
            rows: 0,
            base_seq: 1,
        }),
    );
    apply_ok(
        &engine,
        EngineEvent::Stream(FrameKind::BootstrapChunk {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: payload.into(),
        }),
    );
    assert_eq!(engine.native_agent_view(&agent), None);
    apply_ok(
        &engine,
        EngineEvent::Stream(FrameKind::BootstrapReady {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        }),
    );
    assert_eq!(engine.native_agent_view(&agent), Some(view.clone()));
    apply_ok(&engine, EngineEvent::closed_unknown(agent.clone()));
    let retained = engine.native_agent_view(&agent).unwrap();
    assert!(!retained.connected);
    assert_eq!(retained.activation, view.activation);
    assert_eq!(retained.provider_session_id, None);
}
