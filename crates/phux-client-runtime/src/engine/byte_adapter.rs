//! Bounded synthesized-VT adapter for engine-free builds.

use phux_client_core::engine::{
    BootstrapProgress, CanonicalGeometry, EngineAdapter, EngineDamage, EngineEffect,
    EngineEffectBuffer, HistoryApplyOutcome,
};
use phux_protocol::caps::BootstrapStreamProfile;

/// Bytes one headless replica retains before it refuses more.
pub const HEADLESS_BUFFER_CAP: usize = 1 << 20;

/// An adapter that keeps the raw synthesized-VT bytes of each replica.
#[derive(Debug, Default)]
pub struct ByteAdapter;

/// One headless replica.
#[derive(Debug, Default)]
pub struct ByteReplica {
    /// Every byte applied since the replica started, up to the cap.
    pub bytes: Vec<u8>,
}

/// Why the headless adapter refused.
#[derive(Debug, thiserror::Error)]
pub enum ByteAdapterError {
    /// Only synthesized VT streams have bytes a headless lane can keep.
    #[error("headless adapter only accepts synthesized VT streams")]
    UnsupportedProfile,
    /// The replica outgrew [`HEADLESS_BUFFER_CAP`].
    #[error("headless projection exceeded its bounded byte budget")]
    BufferOverflow,
}

impl ByteAdapter {
    fn append(replica: &mut ByteReplica, payload: &[u8]) -> Result<(), ByteAdapterError> {
        if replica.bytes.len().saturating_add(payload.len()) > HEADLESS_BUFFER_CAP {
            return Err(ByteAdapterError::BufferOverflow);
        }
        replica.bytes.extend_from_slice(payload);
        Ok(())
    }
}

impl EngineAdapter for ByteAdapter {
    type Replica = ByteReplica;
    type Error = ByteAdapterError;

    fn start_replica(
        &mut self,
        profile: BootstrapStreamProfile,
        _geometry: CanonicalGeometry,
    ) -> Result<Self::Replica, Self::Error> {
        if !matches!(
            profile,
            BootstrapStreamProfile::SynthesizedVtRaw
                | BootstrapStreamProfile::SynthesizedVtStateSync
        ) {
            return Err(ByteAdapterError::UnsupportedProfile);
        }
        Ok(ByteReplica::default())
    }

    fn apply_bootstrap_chunk(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        Self::append(replica, payload)?;
        Ok(BootstrapProgress::Pending)
    }

    fn bootstrap_staging_bytes(&self, replica: &Self::Replica) -> usize {
        replica.bytes.capacity()
    }

    fn finish_bootstrap(
        &mut self,
        _replica: &mut Self::Replica,
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        Ok(BootstrapProgress::Finished)
    }

    fn apply_history_page(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        declared_rows: u32,
        _effects: &mut EngineEffectBuffer,
    ) -> Result<HistoryApplyOutcome, Self::Error> {
        Self::append(replica, payload)?;
        Ok(HistoryApplyOutcome {
            progress: BootstrapProgress::Ready,
            retained_rows: declared_rows as usize,
            authenticated_rows: declared_rows as usize,
        })
    }

    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error> {
        Self::append(replica, payload)?;
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        Ok(())
    }
}
