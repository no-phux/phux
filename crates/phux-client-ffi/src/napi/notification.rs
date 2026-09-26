//! Runtime callbacks never touch JS values directly. A weak TSFN queues a
//! coalesced wake on its owning environment. It does not keep Node alive.

use std::sync::Mutex;

use ::napi::bindgen_prelude::Function;
use ::napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use phux_client_runtime::Listener;

use super::registry::lock;

type Callback = ThreadsafeFunction<String, (), String, ::napi::Status, false, true, 1>;

#[derive(Default)]
pub(super) struct Wake {
    callback: Mutex<Option<(String, Callback)>>,
}

impl Wake {
    pub(super) fn install(
        &self,
        handle: String,
        callback: &Function<'_, String, ()>,
    ) -> ::napi::Result<()> {
        let mut slot = lock(&self.callback).map_err(super::registry_error)?;
        if slot.is_some() {
            return Err(::napi::Error::from_reason("ListenerAlreadyInstalled"));
        }
        let callback = callback
            .build_threadsafe_function::<String>()
            .callee_handled::<false>()
            .weak::<true>()
            .max_queue_size::<1>()
            .build()?;
        *slot = Some((handle, callback));
        drop(slot);
        Ok(())
    }

    pub(super) fn close(&self) {
        if let Ok(mut slot) = self.callback.lock() {
            slot.take();
        }
    }
}

impl Listener for Wake {
    fn on_activity(&self) {
        let Ok(slot) = self.callback.lock() else {
            return;
        };
        let Some((handle, callback)) = slot.as_ref() else {
            return;
        };
        // QueueFull means an equivalent wake is already queued. Closing means
        // the JS environment is tearing down. Neither justifies a blind retry.
        let _ = callback.call(handle.clone(), ThreadsafeFunctionCallMode::NonBlocking);
    }
}
