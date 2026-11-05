//! This consumer must compile without a native terminal or a PTY dependency.
use phux_terminal_api::{
    EngineBuild, EngineBundle, HostMetrics, Presentation, TerminalActorError, TerminalEngineFactory,
};

#[derive(Debug)]
struct FakeEngine;
impl TerminalEngineFactory for FakeEngine {
    fn build(&self, request: EngineBuild) -> Result<EngineBundle, TerminalActorError> {
        let (core, channels) = phux_resource::ResourceCore::new(
            phux_resource::ResourceKind::Terminal,
            None,
            request.token.clone(),
            8,
        );
        let facet =
            phux_terminal_api::actor::TerminalHandle::detached_for_test(request.cols, request.rows);
        let handle = phux_resource::ResourceHandle::new(None, channels.output, facet);
        let (_, program_status) = tokio::sync::watch::channel(Vec::new());
        let (_, events) = phux_resource::event_sink::event_sink(request.reports.event_capacity);
        Ok(EngineBundle {
            handle,
            token: request.token,
            run: Box::pin(async move {
                drop(core);
            }),
            exit_notify: Some(channels.exit_notify),
            proc: None,
            events,
            agent_state: None,
            program_status,
        })
    }
    fn bootstrap_capabilities(&self) -> phux_protocol::caps::BootstrapCapabilities {
        phux_protocol::caps::BootstrapCapabilities::new()
    }
    fn metrics(&self) -> &'static [phux_perf::Metric] {
        &[]
    }
    fn host_metrics(&self) -> Option<HostMetrics> {
        None
    }
    fn presentation(&self) -> Presentation {
        Presentation::detached()
    }
}
fn main() {
    let _runtime = phux_server::ServerRuntime::new(
        phux_server::ServerConfig::with_default_socket(),
        std::sync::Arc::new(FakeEngine),
    );
}
