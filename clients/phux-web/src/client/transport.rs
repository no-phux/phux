//! Outbound transports and the DOM bindings a connection owns.

use super::*;

impl WireTx {
    pub(super) fn send(&self, frame: &[u8]) -> Result<(), String> {
        match self {
            Self::Ws(tx) => tx.send(frame),
            Self::Wt(tx) => tx.enqueue(frame),
        }
    }

    pub(super) fn install_failure_hook(&self, hook: Rc<dyn Fn(String)>) {
        match self {
            Self::Ws(tx) => tx.failure.install(hook),
            Self::Wt(tx) => tx.failure.install(hook),
        }
    }

    pub(super) fn close(&self) {
        match self {
            Self::Ws(tx) => {
                let _ = tx.socket.close();
            }
            Self::Wt(tx) => {
                tx.shutdown();
            }
        }
    }
}

impl FailureHook {
    pub(super) fn install(&self, hook: Rc<dyn Fn(String)>) {
        self.0.replace(Some(hook));
    }

    fn notify(&self, message: &str) {
        let hook = self.0.borrow().clone();
        if let Some(hook) = hook {
            hook(message.to_owned());
        }
    }
}

impl WsTx {
    pub(super) fn new(socket: WebSocket) -> Self {
        Self {
            socket,
            failure: FailureHook::default(),
        }
    }

    pub(super) fn send(&self, frame: &[u8]) -> Result<(), String> {
        if self.socket.ready_state() != WebSocket::OPEN {
            return Err("WebSocket is not open".to_owned());
        }
        let queued = self.socket.buffered_amount() as usize;
        if queued.saturating_add(frame.len()) > MAX_OUTBOUND_BYTES {
            return Err("WebSocket outbound byte budget exceeded".to_owned());
        }
        self.socket
            .send_with_u8_array(frame)
            .map_err(|_| "WebSocket send failed".to_owned())
    }
}

impl WtTx {
    pub(super) fn new(writer: WritableStreamDefaultWriter, session: WebTransport) -> Self {
        Self {
            writer,
            session,
            queue: RefCell::new(OutboundQueue::default()),
            writing: Cell::new(false),
            failed: Cell::new(false),
            cancel: RefCell::new(None),
            failure: FailureHook::default(),
        }
    }

    fn enqueue(self: &Rc<Self>, frame: &[u8]) -> Result<(), String> {
        if self.failed.get() {
            return Err("WebTransport writer is closed".to_owned());
        }
        self.queue.borrow_mut().push(frame)?;
        if !self.writing.replace(true) {
            Rc::clone(self).spawn_writer();
        }
        Ok(())
    }

    fn spawn_writer(self: Rc<Self>) {
        let (cancel_tx, cancel_rx) = oneshot::channel();
        self.cancel.replace(Some(cancel_tx));
        wasm_bindgen_futures::spawn_local(async move {
            self.write_queued(cancel_rx).await;
        });
    }

    async fn write_queued(&self, mut cancel: oneshot::Receiver<()>) {
        loop {
            let Some(frame) = self.queue.borrow_mut().pop() else {
                self.finish_writer();
                return;
            };
            let PromiseOutcome::Completed(ready, next_cancel) =
                await_or_cancel(self.writer.ready(), cancel).await
            else {
                self.finish_writer();
                return;
            };
            cancel = next_cancel;
            if ready.is_err() {
                self.fail("WebTransport write failed");
                return;
            }
            let chunk = js_sys::Uint8Array::from(frame.as_slice());
            let PromiseOutcome::Completed(written, next_cancel) =
                await_or_cancel(self.writer.write_with_chunk(&chunk), cancel).await
            else {
                self.finish_writer();
                return;
            };
            cancel = next_cancel;
            if written.is_err() {
                self.fail("WebTransport write failed");
                return;
            }
            self.queue.borrow_mut().complete(frame.len());
        }
    }

    pub(super) fn fail(&self, message: &str) {
        if self.failed.get() {
            return;
        }
        self.shutdown();
        self.failure.notify(message);
    }

    fn shutdown(&self) {
        if self.failed.replace(true) {
            return;
        }
        self.queue.borrow_mut().clear();
        if let Some(cancel) = self.cancel.borrow_mut().take() {
            let _ = cancel.send(());
        }
        self.session.close();
        let pending = self.writer.abort();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = JsFuture::from(pending).await;
        });
    }

    fn finish_writer(&self) {
        self.cancel.borrow_mut().take();
        self.writing.set(false);
    }
}

pub(super) async fn await_or_cancel<T: wasm_bindgen::convert::FromWasmAbi + 'static>(
    promise: js_sys::Promise<T>,
    cancel: oneshot::Receiver<()>,
) -> PromiseOutcome<T> {
    match select(cancel, Box::pin(JsFuture::from(promise))).await {
        Either::Left(_) => PromiseOutcome::Cancelled,
        Either::Right((result, cancel)) => PromiseOutcome::Completed(result, cancel),
    }
}

impl OutboundQueue {
    pub(super) fn push(&mut self, frame: &[u8]) -> Result<(), String> {
        let next_bytes = self.bytes.saturating_add(frame.len());
        if next_bytes > MAX_OUTBOUND_BYTES {
            return Err("WebTransport outbound byte budget exceeded".to_owned());
        }
        self.frames.push_back(frame.to_vec());
        self.bytes = next_bytes;
        Ok(())
    }

    pub(super) fn pop(&mut self) -> Option<Vec<u8>> {
        self.frames.pop_front()
    }

    pub(super) fn complete(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_sub(bytes);
    }

    pub(super) fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }
}

impl AppBindings {
    pub(super) fn dispose(&mut self) {
        if let Some(websocket) = self.websocket.take() {
            websocket.dispose();
        }
        if let Some(input) = self.input.take() {
            input.dispose();
        }
        if let Some(find) = self.find.take() {
            find.dispose();
        }
        if let Some(picker) = self.path_picker.take() {
            picker.dispose();
        }
        if let Some(blink) = self.blink.take() {
            blink.dispose();
        }
        if let Some(frame) = self.frame.take() {
            frame.dispose();
        }
        if let Some(expiry) = self.bootstrap_expiry.take() {
            expiry.dispose();
        }
        if let Some(cancel) = self.wt_reader_cancel.take() {
            let _ = cancel.send(());
        }
    }
}

impl WebSocketBindings {
    pub(super) fn dispose(self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onerror(None);
        self.socket.set_onclose(None);
        drop((self.onopen, self.onmessage, self.onerror, self.onclose));
    }
}

impl InputBinding {
    pub(super) fn dispose(self) {
        self.listeners.dispose();
        self.surface.remove();
    }
}

impl Listeners {
    pub(super) fn listen(
        &mut self,
        target: &web_sys::EventTarget,
        kind: &'static str,
        handler: impl FnMut(web_sys::Event) + 'static,
    ) -> Result<(), JsValue> {
        let callback = Closure::<dyn FnMut(web_sys::Event)>::new(handler);
        target.add_event_listener_with_callback(kind, callback.as_ref().unchecked_ref())?;
        self.0.push(Listener {
            target: target.clone(),
            kind,
            callback,
        });
        Ok(())
    }

    pub(super) fn dispose(self) {
        for listener in self.0 {
            let _ = listener.target.remove_event_listener_with_callback(
                listener.kind,
                listener.callback.as_ref().unchecked_ref(),
            );
        }
    }
}

impl FrameBinding {
    pub(super) fn request(&self) {
        if self.pending.get().is_some() {
            return;
        }
        if let Ok(id) = self
            .window
            .request_animation_frame(self.callback.as_ref().unchecked_ref())
        {
            self.pending.set(Some(id));
        }
    }

    pub(super) fn dispose(self) {
        if let Some(id) = self.pending.take() {
            let _ = self.window.cancel_animation_frame(id);
        }
        drop(self.callback);
    }
}

impl BlinkBinding {
    pub(super) fn dispose(self) {
        self.window.clear_interval_with_handle(self.interval);
        drop(self.callback);
    }
}
