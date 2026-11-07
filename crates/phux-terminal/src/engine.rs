//! Concrete composition boundary. No server types cross into this module.
use phux_terminal_api::{
    AdaptedBootstrap, EngineBuild, EngineBundle, EngineSource, TerminalEngineFactory,
};

#[derive(Debug, Default)]
pub struct TerminalEngine;

fn command(intent: &phux_terminal_api::launch::LaunchCommand) -> portable_pty::CommandBuilder {
    let mut command = portable_pty::CommandBuilder::from_argv(intent.get_argv().clone());
    // portable-pty supplies SHELL from passwd when it was not inherited. Preserve
    // that fallback, but never resurrect an explicitly removed environment key.
    let shell = (!intent.was_removed("SHELL"))
        .then(|| command.get_env("SHELL").map(std::ffi::OsStr::to_owned))
        .flatten();
    command.env_clear();
    if let Some(shell) = shell {
        command.env("SHELL", shell);
    }
    for (key, value) in intent.environment() {
        command.env(key, value);
    }
    if let Some(cwd) = intent.get_cwd() {
        command.cwd(cwd);
    }
    command.set_controlling_tty(intent.get_controlling_tty());
    command
}

impl TerminalEngineFactory for TerminalEngine {
    fn build(
        &self,
        request: EngineBuild,
    ) -> Result<EngineBundle, phux_terminal_api::TerminalActorError> {
        let EngineBuild {
            cols,
            rows,
            source,
            scrollback,
            token,
            default_colors,
            reports,
        } = request;
        if reports.event_capacity == 0 || reports.agent_capacity == 0 {
            return Err(phux_terminal_api::TerminalActorError::Refused(
                "engine report capacities must be nonzero".into(),
            ));
        }
        let bundle = match source {
            EngineSource::Fresh(intent) => crate::TerminalActor::build_with_token_and_colors(
                cols,
                rows,
                intent.as_ref().map(command),
                scrollback,
                token,
                default_colors,
            ),
            EngineSource::Adopt {
                master_fd,
                child_pid,
                seed,
            } => crate::TerminalActor::new_with_adopted_pty(
                master_fd, child_pid, cols, rows, scrollback, token, &seed,
            ),
            EngineSource::Replay(seed) => crate::TerminalActor::build_with_token_and_colors(
                cols,
                rows,
                None,
                scrollback,
                token,
                default_colors,
            )
            .map(|mut bundle| {
                bundle.actor.seed_before_run(&seed);
                bundle
            }),
        }
        .map_err(|error| phux_terminal_api::TerminalActorError::Launch {
            message: error.to_string(),
            diagnostic: error.launch_diagnostic(),
        })?;
        let crate::TerminalActorBundle {
            mut actor,
            handle,
            token,
            exit_notify,
            proc,
        } = bundle;
        let (event_sink, events) = phux_resource::event_sink::event_sink(reports.event_capacity);
        actor.set_event_sink(event_sink);
        let agent_state = actor.has_pty().then(|| {
            let (sink, source) = tokio::sync::mpsc::channel(reports.agent_capacity);
            actor.set_agent_state_sink(sink);
            if let Some(probe) = reports.live_session {
                actor.set_live_session_probe(probe);
            }
            source
        });
        let program_status = actor.subscribe_program_status();
        Ok(EngineBundle {
            handle,
            token,
            run: Box::pin(actor.run()),
            exit_notify,
            proc,
            events,
            agent_state,
            program_status,
        })
    }
    fn bootstrap_capabilities(&self) -> phux_protocol::caps::BootstrapCapabilities {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            crate::native_state::native_bootstrap_capabilities()
        }
        #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
        {
            phux_protocol::caps::BootstrapCapabilities::new()
        }
    }
    fn metrics(&self) -> &'static [phux_perf::Metric] {
        crate::perf::TABLE
    }
    fn host_metrics(&self) -> Option<phux_terminal_api::HostMetrics> {
        Some(phux_terminal_api::HostMetrics {
            input_credit_waits: &crate::perf::INPUT_CREDIT_WAITS,
            input_credit_wait: &crate::perf::INPUT_CREDIT_WAIT,
            input_credit_timeouts: &crate::perf::INPUT_CREDIT_TIMEOUTS,
            agent_publish: &crate::perf::AGENT_PUBLISH,
        })
    }
    fn presentation(&self) -> phux_terminal_api::Presentation {
        phux_terminal_api::Presentation {
            adapt_bytes: Self::adapt_bytes,
            source_ceiling: Self::bootstrap_source_ceiling,
            adapt_snapshot,
        }
    }
}
impl TerminalEngine {
    fn adapt_bytes(bytes: &bytes::Bytes, caps: phux_protocol::ClientCapabilities) -> bytes::Bytes {
        if crate::downsample::caps_pass_through(caps) || memchr::memchr(0x1b, bytes).is_none() {
            bytes.clone()
        } else {
            crate::downsample::rewrite_bytes_with_caps(bytes, caps).into()
        }
    }
    fn bootstrap_source_ceiling(
        remaining: usize,
        caps: phux_protocol::ClientCapabilities,
    ) -> usize {
        if crate::downsample::caps_pass_through(caps) {
            remaining
        } else {
            remaining / 2
        }
    }
}

fn adapt_snapshot(
    snapshot: phux_terminal_api::grid::SnapshotBytes,
    caps: phux_protocol::ClientCapabilities,
    peak_budget: usize,
) -> Result<AdaptedBootstrap, ()> {
    let sources = [snapshot.scrollback, snapshot.bytes];
    let mut remaining_source = sources
        .iter()
        .try_fold(0_usize, |total, source| {
            total.checked_add(source.capacity())
        })
        .ok_or(())?;
    if remaining_source > TerminalEngine::bootstrap_source_ceiling(peak_budget, caps) {
        return Err(());
    }
    let mut peak_bytes = remaining_source;
    let mut retained_output = 0_usize;
    let mut payloads = Vec::new();
    payloads.try_reserve(2).map_err(|_| ())?;
    for source in sources {
        let capacity = source.capacity();
        if source.is_empty() {
            remaining_source = remaining_source.checked_sub(capacity).ok_or(())?;
            continue;
        }
        let (output, allocation) = adapt_source(
            source,
            caps,
            retained_output,
            remaining_source,
            peak_budget,
            &mut peak_bytes,
        )?;
        remaining_source = remaining_source.checked_sub(capacity).ok_or(())?;
        retained_output = retained_output.checked_add(allocation).ok_or(())?;
        payloads.push(output);
    }
    Ok(AdaptedBootstrap {
        payloads,
        retained_bytes: retained_output,
        peak_bytes,
    })
}

fn adapt_source(
    source: Vec<u8>,
    caps: phux_protocol::ClientCapabilities,
    retained: usize,
    remaining: usize,
    budget: usize,
    peak: &mut usize,
) -> Result<(bytes::Bytes, usize), ()> {
    let capacity = source.capacity();
    if crate::downsample::caps_pass_through(caps) {
        return Ok((source.into(), capacity));
    }
    let rewritten = crate::downsample::rewrite_bytes_with_caps(&source, caps);
    let allocation = rewritten.capacity();
    if allocation > capacity {
        return Err(());
    }
    let live = retained
        .checked_add(remaining)
        .and_then(|bytes| bytes.checked_add(allocation))
        .ok_or(())?;
    if live > budget {
        return Err(());
    }
    *peak = (*peak).max(live);
    drop(source);
    Ok((rewritten.into(), allocation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_terminal_api::launch::LaunchCommand;

    #[test]
    fn launch_intent_preserves_os_arguments_environment_and_cwd() {
        use std::os::unix::ffi::OsStringExt;
        let opaque = std::ffi::OsString::from_vec(b"non-utf8-\xff".to_vec());
        let mut intent = LaunchCommand::new("/bin/echo");
        intent.arg(&opaque);
        intent.cwd(&opaque);
        intent.env("OPAQUE", &opaque);
        intent.env_remove("SHELL");
        intent.set_controlling_tty(false);
        let native = command(&intent);
        assert_eq!(native.get_argv()[1], opaque);
        assert_eq!(native.get_cwd(), Some(&opaque));
        assert_eq!(native.get_env("OPAQUE"), Some(opaque.as_os_str()));
        assert_eq!(native.get_env("SHELL"), None);
        assert!(!native.get_controlling_tty());
    }

    #[test]
    fn clearing_launch_environment_is_not_replaced_by_inheritance() {
        let mut intent = LaunchCommand::new("/bin/echo");
        intent.env_clear();
        intent.env("ONLY", "present");
        let native = command(&intent);
        assert_eq!(
            native.get_env("ONLY"),
            Some(std::ffi::OsStr::new("present"))
        );
        assert_eq!(native.get_env("PATH"), None);
        assert_eq!(native.get_env("SHELL"), None);
    }

    #[test]
    fn factory_keeps_resource_generation_and_agent_parent_companion() {
        use phux_terminal_api::actor::TerminalResource as _;
        let token = tokio_util::sync::CancellationToken::new();
        let bundle = TerminalEngine
            .build(EngineBuild {
                cols: 80,
                rows: 24,
                source: EngineSource::Fresh(None),
                scrollback: phux_config::ScrollbackLimits::default(),
                token: token.clone(),
                default_colors: None,
                reports: phux_terminal_api::EngineReports {
                    event_capacity: 1,
                    agent_capacity: 1,
                    live_session: None,
                },
            })
            .expect("engine");
        assert!(
            bundle
                .handle
                .companion::<phux_agent_session::parent::AgentParent>()
                .is_some()
        );
        let clone = bundle.handle.clone();
        let first = &bundle.handle.terminal().expect("terminal").ordered_input;
        assert!(first.same_generation(&clone.terminal().expect("same facet").ordered_input));
        token.cancel();
        assert!(bundle.token.is_cancelled());
    }
}
