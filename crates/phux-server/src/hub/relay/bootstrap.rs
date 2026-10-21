//! Bootstrap flow per relayed generation: profile checks, chunk acceptance, and identity validation.

use super::{
    BootstrapId, BootstrapProfile, BootstrapStreamProfile, MAX_RELAY_GENERATION_BYTES,
    MAX_RELAY_GENERATION_FRAMES, RelayBootstrapFlow, RelaySession, ServerFeatureExt, StreamId,
    charge_bytes, charge_frame, ensure_chunk_identity, ensure_chunk_sequence,
};

impl RelaySession {
    pub(super) const fn expected_stream_profile(&self) -> Option<BootstrapStreamProfile> {
        match self.bootstrap_profile {
            BootstrapProfile::NativeState { codec, .. } => {
                Some(BootstrapStreamProfile::NativeState { codec })
            }
            BootstrapProfile::SynthesizedVtRaw => Some(BootstrapStreamProfile::SynthesizedVtRaw),
            BootstrapProfile::SynthesizedVtStateSync => {
                Some(BootstrapStreamProfile::SynthesizedVtStateSync)
            }
            _ => None,
        }
    }

    pub(super) fn accepts_stream_profile(&self, profile: BootstrapStreamProfile) -> bool {
        match profile {
            BootstrapStreamProfile::AgentEventsJsonlV1 => true,
            BootstrapStreamProfile::AgentEventsJsonlResumeV1 { .. } => self
                .satellite_features_ext
                .contains(ServerFeatureExt::AgentStreamResume),
            _ => Some(profile) == self.expected_stream_profile(),
        }
    }

    pub(super) fn begin_bootstrap_flow(
        &mut self,
        terminal: u32,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        profile: BootstrapStreamProfile,
    ) -> Result<(), String> {
        if !self.accepts_stream_profile(profile) {
            return Err(format!(
                "satellite {} sent BOOTSTRAP_BEGIN profile {profile:?}, negotiated {:?}",
                self.host, self.bootstrap_profile
            ));
        }
        if self.bootstrap_flows.contains_key(&terminal) {
            return Err(format!(
                "satellite {} sent overlapping BOOTSTRAP_BEGIN for terminal {terminal}",
                self.host
            ));
        }
        if self.inflight_generation_frames >= MAX_RELAY_GENERATION_FRAMES * 2 {
            return Err(format!(
                "satellite {} exceeded the connection-wide in-flight bootstrap frame budget",
                self.host
            ));
        }
        self.inflight_generation_frames += 1;
        self.bootstrap_flows.insert(
            terminal,
            RelayBootstrapFlow {
                stream_id,
                bootstrap_id,
                profile,
                next_chunk_seq: 0,
                generation_bytes: 0,
                generation_frames: 1,
                ready: false,
            },
        );
        Ok(())
    }

    pub(super) fn accept_bootstrap_chunk(
        &mut self,
        terminal: u32,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        chunk_seq: u32,
        payload_len: usize,
    ) -> Result<(), String> {
        let payload_len = u64::try_from(payload_len)
            .map_err(|_| format!("satellite {} sent an oversized bootstrap chunk", self.host))?;
        let Some(flow) = self.bootstrap_flows.get_mut(&terminal) else {
            return Err(format!(
                "satellite {} sent BOOTSTRAP_CHUNK before BEGIN for terminal {terminal}",
                self.host
            ));
        };
        ensure_chunk_identity(&self.host, terminal, flow, stream_id, bootstrap_id)?;
        ensure_chunk_sequence(&self.host, flow, chunk_seq)?;
        let next_frames = charge_frame(
            &self.host,
            "per-generation bootstrap frame budget",
            flow.generation_frames,
            MAX_RELAY_GENERATION_FRAMES,
        )?;
        let next_bytes = charge_bytes(
            &self.host,
            "per-generation bootstrap byte budget",
            flow.generation_bytes,
            payload_len,
            MAX_RELAY_GENERATION_BYTES,
        )?;
        let connection_frames = charge_frame(
            &self.host,
            "connection-wide in-flight bootstrap frame budget",
            self.inflight_generation_frames,
            MAX_RELAY_GENERATION_FRAMES * 2,
        )?;
        let connection_bytes = charge_bytes(
            &self.host,
            "connection-wide in-flight bootstrap byte budget",
            self.inflight_generation_bytes,
            payload_len,
            MAX_RELAY_GENERATION_BYTES * 2,
        )?;
        flow.next_chunk_seq = flow.next_chunk_seq.checked_add(1).ok_or_else(|| {
            format!(
                "satellite {} overflowed the bootstrap chunk sequence",
                self.host
            )
        })?;
        flow.generation_frames = next_frames;
        flow.generation_bytes = next_bytes;
        self.inflight_generation_frames = connection_frames;
        self.inflight_generation_bytes = connection_bytes;
        Ok(())
    }

    pub(super) fn finish_bootstrap_flow(
        &mut self,
        terminal: u32,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
    ) -> Result<(), String> {
        let Some(flow) = self.bootstrap_flows.get_mut(&terminal) else {
            return Err(format!(
                "satellite {} sent BOOTSTRAP_READY before BEGIN for terminal {terminal}",
                self.host
            ));
        };
        if flow.ready || flow.stream_id != stream_id || flow.bootstrap_id != bootstrap_id {
            return Err(format!(
                "satellite {} changed or reused bootstrap identity at READY for terminal {terminal}",
                self.host
            ));
        }
        if flow.generation_frames >= MAX_RELAY_GENERATION_FRAMES {
            return Err(format!(
                "satellite {} exceeded the per-generation bootstrap frame budget",
                self.host
            ));
        }
        flow.generation_frames += 1;
        flow.ready = true;
        self.inflight_generation_bytes = self
            .inflight_generation_bytes
            .saturating_sub(flow.generation_bytes);
        self.inflight_generation_frames = self
            .inflight_generation_frames
            .saturating_sub(flow.generation_frames.saturating_sub(1));
        Ok(())
    }
    pub(super) fn validate_known_identity(
        &self,
        terminal: u32,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        frame_name: &str,
    ) -> Result<(), String> {
        let Some(flow) = self.bootstrap_flows.get(&terminal) else {
            return Err(format!(
                "satellite {} sent {frame_name} without a selected bootstrap generation for terminal {terminal}",
                self.host
            ));
        };
        if flow.stream_id != stream_id
            || flow.bootstrap_id != bootstrap_id
            || !self.accepts_stream_profile(flow.profile)
        {
            return Err(format!(
                "satellite {} sent {frame_name} for a non-active bootstrap identity on terminal {terminal}",
                self.host
            ));
        }
        Ok(())
    }

    pub(super) fn validate_ready_identity(
        &self,
        terminal: u32,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        frame_name: &str,
    ) -> Result<(), String> {
        self.validate_known_identity(terminal, stream_id, bootstrap_id, frame_name)?;
        if !self.bootstrap_flows[&terminal].ready {
            return Err(format!(
                "satellite {} sent {frame_name} before BOOTSTRAP_READY for terminal {terminal}",
                self.host
            ));
        }
        Ok(())
    }

    pub(super) fn retire_bootstrap_flow(&mut self, terminal: u32) {
        if let Some(flow) = self.bootstrap_flows.remove(&terminal)
            && !flow.ready
        {
            self.inflight_generation_bytes = self
                .inflight_generation_bytes
                .saturating_sub(flow.generation_bytes);
            self.inflight_generation_frames = self
                .inflight_generation_frames
                .saturating_sub(flow.generation_frames);
        }
    }
}
