//! The browser's engine adapter: wasm VT replicas behind the session kernel.

use super::*;

pub(super) struct WebEngine {
    pub(super) vt: Rc<Vt>,
    pub(super) limits: BootstrapLimits,
}

pub(super) struct WebReplica {
    state: WebReplicaState,
    history_budget: Option<(usize, usize)>,
}

pub(super) enum WebReplicaState {
    Synthesized(Terminal),
    /// The decoder owns the READY terminal; `protocol_finished` records
    /// that the protocol READY decoded and published it.
    Native {
        decoder: NativeDecoder,
        protocol_finished: bool,
    },
}

impl WebReplica {
    pub(super) fn terminal(&self) -> Option<&Terminal> {
        match &self.state {
            WebReplicaState::Synthesized(terminal) => Some(terminal),
            WebReplicaState::Native { decoder, .. } => decoder.terminal(),
        }
    }

    fn apply_history_budget(&self) -> Result<(), WebEngineError> {
        if let (Some(terminal), Some((max_bytes, max_rows))) =
            (self.terminal(), self.history_budget)
        {
            terminal.set_history_budget(max_bytes, max_rows)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) enum WebEngineError {
    UnsupportedProfile(BootstrapStreamProfile),
    Native(NativeCodecError),
    PayloadLimitExceeded { actual: usize, limit: usize },
    InvalidNativeTransition(&'static str),
}

impl From<NativeCodecError> for WebEngineError {
    fn from(error: NativeCodecError) -> Self {
        Self::Native(error)
    }
}

impl std::fmt::Display for WebEngineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedProfile(profile) => {
                write!(formatter, "unsupported web bootstrap profile: {profile:?}")
            }
            Self::Native(error) => write!(formatter, "{error}"),
            Self::PayloadLimitExceeded { actual, limit } => {
                write!(
                    formatter,
                    "engine payload is {actual} bytes; limit is {limit}"
                )
            }
            Self::InvalidNativeTransition(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for WebEngineError {}

impl EngineAdapter for WebEngine {
    type Replica = WebReplica;
    type Error = WebEngineError;

    fn start_replica(
        &mut self,
        profile: BootstrapStreamProfile,
        geometry: CanonicalGeometry,
    ) -> Result<Self::Replica, Self::Error> {
        let state = match profile {
            BootstrapStreamProfile::SynthesizedVtRaw
            | BootstrapStreamProfile::SynthesizedVtStateSync => {
                WebReplicaState::Synthesized(self.vt.terminal(geometry.cols, geometry.rows))
            }
            BootstrapStreamProfile::NativeState {
                codec: EngineCodec::LibghosttySnapshotV1,
            } if self.vt.native_snapshot_available() => WebReplicaState::Native {
                decoder: self.vt.native_decoder(NATIVE_DECODER_LIMIT)?,
                protocol_finished: false,
            },
            _ => return Err(WebEngineError::UnsupportedProfile(profile)),
        };
        Ok(WebReplica {
            state,
            history_budget: None,
        })
    }

    fn configure_history_budget(
        &mut self,
        replica: &mut Self::Replica,
        max_bytes: usize,
        max_rows: usize,
    ) -> Result<(), Self::Error> {
        replica.history_budget = Some((max_bytes.max(1), max_rows.max(1)));
        replica.apply_history_budget()
    }

    fn apply_bootstrap_chunk(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        let limit = self.limits.max_chunk_bytes() as usize;
        if payload.len() > limit {
            return Err(WebEngineError::PayloadLimitExceeded {
                actual: payload.len(),
                limit,
            });
        }
        match &mut replica.state {
            WebReplicaState::Synthesized(terminal) => {
                terminal.write(payload);
                // Replayed state is not the program ringing now.
                let _ = terminal.take_bell();
                effects.push(EngineEffect::Damage(EngineDamage::Full));
                Ok(BootstrapProgress::Pending)
            }
            WebReplicaState::Native {
                decoder,
                protocol_finished,
            } => {
                if *protocol_finished {
                    return Err(WebEngineError::InvalidNativeTransition(
                        "bootstrap input arrived after READY",
                    ));
                }
                // The prefix is decoded whole at protocol READY, so a chunk
                // boundary can never be mistaken for end-of-file.
                decoder.feed(payload)?;
                Ok(BootstrapProgress::Pending)
            }
        }
    }

    fn bootstrap_staging_bytes(&self, replica: &Self::Replica) -> usize {
        match &replica.state {
            WebReplicaState::Native { decoder, .. } => decoder.staged_capacity(),
            WebReplicaState::Synthesized(_) => 0,
        }
    }

    fn finish_bootstrap(
        &mut self,
        replica: &mut Self::Replica,
        effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        let progress = match &mut replica.state {
            WebReplicaState::Synthesized(_) => BootstrapProgress::Finished,
            WebReplicaState::Native {
                decoder,
                protocol_finished,
            } => {
                if std::mem::replace(protocol_finished, true) {
                    return Err(WebEngineError::InvalidNativeTransition(
                        "protocol READY arrived twice",
                    ));
                }
                decoder.ready()?;
                // Renderable now; history pages and FINISH follow.
                BootstrapProgress::Ready
            }
        };
        replica.apply_history_budget()?;
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        Ok(progress)
    }

    fn apply_history_page(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        declared_rows: u32,
        _effects: &mut EngineEffectBuffer,
    ) -> Result<HistoryApplyOutcome, Self::Error> {
        let limit = self.limits.max_history_page_bytes() as usize;
        if payload.len() > limit {
            return Err(WebEngineError::PayloadLimitExceeded {
                actual: payload.len(),
                limit,
            });
        }
        let WebReplicaState::Native {
            decoder,
            protocol_finished,
        } = &mut replica.state
        else {
            return Ok(HistoryApplyOutcome {
                progress: BootstrapProgress::Finished,
                retained_rows: declared_rows as usize,
                authenticated_rows: declared_rows as usize,
            });
        };
        if !*protocol_finished {
            return Err(WebEngineError::InvalidNativeTransition(
                "history arrived before protocol READY",
            ));
        }
        decoder.feed(payload)?;
        let outcome = match decoder.next_checked(declared_rows as usize)? {
            Some(page) => HistoryApplyOutcome {
                progress: BootstrapProgress::Ready,
                retained_rows: page.rows,
                authenticated_rows: page.authenticated_rows,
            },
            None => HistoryApplyOutcome {
                progress: BootstrapProgress::Finished,
                retained_rows: 0,
                authenticated_rows: 0,
            },
        };
        Ok(outcome)
    }

    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error> {
        let terminal = replica
            .terminal()
            .ok_or(WebEngineError::InvalidNativeTransition(
                "live output arrived before READY",
            ))?;
        terminal.write(payload);
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        if terminal.take_bell() {
            effects.push(EngineEffect::Status(EngineStatus::Bell));
        }
        Ok(())
    }
}
