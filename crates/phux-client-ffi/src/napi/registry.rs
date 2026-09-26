//! Session identities are never reused, even after close. The counter fails
//! closed on exhaustion rather than wrapping into a stale identity.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use phux_client_runtime::control::Event;
use phux_client_runtime::{Client, ClientOptions, Runtime, Target};

use super::notification::Wake;

/// Stable registry failures; diagnostics are not a command protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    /// This identity is absent, malformed, or was closed.
    StaleHandle,
    /// The created session has not connected yet.
    NotConnected,
    /// Each identity can connect only once.
    AlreadyConnected,
    /// A poisoned lock is never silently recovered.
    Poisoned,
    /// No more non-reusable identities can be allocated.
    Exhausted,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for RegistryError {}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, RegistryError> {
    mutex.lock().map_err(|_| RegistryError::Poisoned)
}

#[derive(Default)]
struct Entries {
    next: u64,
    sessions: HashMap<String, Arc<Session>>,
    environments: HashSet<usize>,
}

/// One registry in the final loaded host. Never load a second FFI dylib next
/// to the desktop wrapper: that would create a second registry.
#[derive(Default)]
pub struct Registry {
    entries: Mutex<Entries>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry").finish_non_exhaustive()
    }
}

/// Initialize the process registry idempotently. The desktop wrapper should
/// call this Rust symbol to link this crate into the same host as its painter.
#[must_use]
pub fn initialize() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::default)
}

impl Registry {
    /// Register one cleanup hook per live NAPI environment, not per client.
    /// The key is compared only; it is never dereferenced as a pointer.
    pub(super) fn register_environment(
        &self,
        key: usize,
        install: impl FnOnce() -> ::napi::Result<()>,
    ) -> ::napi::Result<()> {
        let mut entries = lock(&self.entries).map_err(super::registry_error)?;
        if entries.environments.contains(&key) {
            return Ok(());
        }
        install()?;
        entries.environments.insert(key);
        drop(entries);
        Ok(())
    }

    pub(super) fn close_environment(&self, key: usize) -> Result<(), RegistryError> {
        let sessions: Vec<_> = {
            let mut entries = lock(&self.entries)?;
            entries.environments.remove(&key);
            entries
                .sessions
                .extract_if(|_, session| session.environment == Some(key))
                .map(|(_, session)| session)
                .collect()
        };
        for session in sessions {
            session.close()?;
        }
        Ok(())
    }

    pub(super) fn create(&self, environment: Option<usize>) -> Result<String, RegistryError> {
        let mut entries = lock(&self.entries)?;
        entries.next = entries
            .next
            .checked_add(1)
            .ok_or(RegistryError::Exhausted)?;
        let handle = format!("phux-client:{}", entries.next);
        entries.sessions.insert(
            handle.clone(),
            Arc::new(Session {
                environment,
                state: Mutex::default(),
                wake: Arc::default(),
            }),
        );
        drop(entries);
        Ok(handle)
    }

    pub(super) fn session(&self, handle: &str) -> Result<Arc<Session>, RegistryError> {
        lock(&self.entries)?
            .sessions
            .get(handle)
            .cloned()
            .ok_or(RegistryError::StaleHandle)
    }

    /// Obtain the actual shared runtime client for a native painter.
    ///
    /// The clone is an in-flight lease, not a new connection. Close invalidates
    /// future lookups and closes this same client, including outstanding clones.
    /// The binding exclusively owns `set_listener` and `take_events`; native
    /// consumers must not call either on this lease. Frames stay in Rust.
    ///
    /// # Errors
    /// Returns `StaleHandle`, `NotConnected`, or `Poisoned`.
    pub fn client(&self, handle: &str) -> Result<Client, RegistryError> {
        self.session(handle)?.client()
    }

    /// Invalidate the identity, close its connection, and return all undrained
    /// events including final acknowledged-input outcomes. Runtime close
    /// synchronously settles unresolved deliveries; this is their final drain.
    /// A second close is a deterministic `StaleHandle`, never an operation on
    /// a reused slot.
    ///
    /// # Errors
    /// Returns `StaleHandle` or `Poisoned`.
    pub fn close(&self, handle: &str) -> Result<Vec<Event>, RegistryError> {
        let session = lock(&self.entries)?
            .sessions
            .remove(handle)
            .ok_or(RegistryError::StaleHandle)?;
        session.close()
    }
}

#[derive(Default)]
struct State {
    closed: bool,
    client: Option<Client>,
}

#[derive(Default)]
pub(super) struct Session {
    environment: Option<usize>,
    state: Mutex<State>,
    pub(super) wake: Arc<Wake>,
}

impl Session {
    pub(super) fn connect(&self, target: Target, options: ClientOptions) -> ::napi::Result<()> {
        let mut state = lock(&self.state).map_err(super::registry_error)?;
        if state.closed {
            return Err(super::registry_error(RegistryError::StaleHandle));
        }
        if state.client.is_some() {
            return Err(super::registry_error(RegistryError::AlreadyConnected));
        }
        let client = Runtime::connect(target, options)
            .map_err(|error| ::napi::Error::from_reason(error.to_string()))?;
        client.set_listener(self.wake.clone());
        state.client = Some(client);
        drop(state);
        Ok(())
    }

    pub(super) fn client(&self) -> Result<Client, RegistryError> {
        let state = lock(&self.state)?;
        if state.closed {
            return Err(RegistryError::StaleHandle);
        }
        state.client.clone().ok_or(RegistryError::NotConnected)
    }

    fn close(&self) -> Result<Vec<Event>, RegistryError> {
        let client = {
            let mut state = lock(&self.state)?;
            state.closed = true;
            state.client.take()
        };
        self.wake.close();
        if let Some(client) = client {
            client.close();
            return Ok(client.take_events());
        }
        Ok(Vec::new())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_handles_never_alias_and_missing_connections_are_explicit() {
        let registry = Registry::default();
        let first = registry.create(None).expect("create");
        assert_eq!(
            registry.client(&first).unwrap_err(),
            RegistryError::NotConnected
        );
        registry.close(&first).expect("close");
        let second = registry.create(None).expect("create again");
        assert_ne!(first, second);
        assert_eq!(
            registry.client(&first).unwrap_err(),
            RegistryError::StaleHandle
        );
        assert!(matches!(
            registry.close(&first),
            Err(RegistryError::StaleHandle)
        ));
        registry.close(&second).expect("close second");
    }

    #[test]
    fn exhaustion_does_not_wrap_and_ids_exceed_js_safe_integers_losslessly() {
        let registry = Registry::default();
        lock(&registry.entries).expect("lock").next = 9_007_199_254_740_991;
        assert_eq!(
            registry.create(None).expect("create"),
            "phux-client:9007199254740992"
        );
        lock(&registry.entries).expect("lock").next = u64::MAX;
        assert_eq!(registry.create(None), Err(RegistryError::Exhausted));
    }

    #[test]
    fn a_lookup_racing_close_cannot_reconnect_the_removed_session() {
        let registry = Registry::default();
        let handle = registry.create(None).expect("create");
        let session = registry.session(&handle).expect("lookup lease");
        registry.close(&handle).expect("close");
        let error = session
            .connect(Target::uds("/unused"), ClientOptions::default())
            .expect_err("closed");
        assert_eq!(error.reason, "StaleHandle");
    }

    #[test]
    fn close_reaches_native_painter_leases_of_the_same_client() {
        let registry = Registry::default();
        let handle = registry.create(None).expect("create");
        let session = registry.session(&handle).expect("session");
        lock(&session.state).expect("state").client = Some(Runtime::embedded(
            phux_client_runtime::control::ControlOptions::default(),
        ));
        let painter = registry.client(&handle).expect("painter lease");
        let other = registry.client(&handle).expect("another lease");
        registry.close(&handle).expect("close");
        assert_eq!(
            painter.status(),
            phux_client_runtime::control::Status::Closed
        );
        assert_eq!(other.status(), phux_client_runtime::control::Status::Closed);
        assert_eq!(
            registry.client(&handle).unwrap_err(),
            RegistryError::StaleHandle
        );
    }

    #[test]
    fn close_returns_undrained_receipts_once_and_invalidates_native_lookups() {
        let registry = Registry::default();
        let handle = registry.create(None).expect("create");
        let session = registry.session(&handle).expect("session");
        let client = Runtime::embedded(phux_client_runtime::control::ControlOptions::default());
        let delivery_id = client.refuse_acknowledged_input("bad-id");
        lock(&session.state).expect("state").client = Some(client.clone());
        let events = registry.close(&handle).expect("close final batch");
        assert_eq!(events.iter().filter(|event| matches!(event,
            Event::InputDelivery { delivery_id: id, outcome: phux_client_runtime::control::DeliveryOutcome::Refused, .. }
                if *id == delivery_id)).count(), 1);
        assert!(
            client.take_events().is_empty(),
            "the final drain consumed the receipt"
        );
        assert_eq!(
            registry.client(&handle).unwrap_err(),
            RegistryError::StaleHandle
        );
        assert!(matches!(
            registry.close(&handle),
            Err(RegistryError::StaleHandle)
        ));
    }

    #[test]
    fn cleanup_registrations_are_bounded_by_live_environments_not_client_churn() {
        let registry = Registry::default();
        let mut registrations = 0;
        for _ in 0..10_000 {
            registry
                .register_environment(7, || {
                    registrations += 1;
                    Ok(())
                })
                .expect("register environment");
            let handle = registry.create(Some(7)).expect("create");
            registry.close(&handle).expect("dispose");
        }
        assert_eq!(registrations, 1);
        assert_eq!(
            lock(&registry.entries).expect("entries").environments.len(),
            1
        );
        assert!(
            lock(&registry.entries)
                .expect("entries")
                .sessions
                .is_empty()
        );
        let abandoned = registry.create(Some(7)).expect("abandoned at env exit");
        let other = registry.create(Some(8)).expect("different environment");
        registry.close_environment(7).expect("env cleanup");
        assert_eq!(
            registry.client(&abandoned).unwrap_err(),
            RegistryError::StaleHandle
        );
        assert!(
            lock(&registry.entries)
                .expect("entries")
                .environments
                .is_empty()
        );
        assert_eq!(
            registry.client(&other).unwrap_err(),
            RegistryError::NotConnected
        );
        registry
            .register_environment(7, || {
                registrations += 1;
                Ok(())
            })
            .expect("recycled env address");
        assert_eq!(registrations, 2, "a new environment gets a fresh hook");
        registry.close(&other).expect("close other");
    }
}
