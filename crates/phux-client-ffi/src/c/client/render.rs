use super::{
    Arc, BridgeError, Client, GridFrame, GridMetadataCache, HistoryLoadState, HistoryStatus,
    PhuxClientEffect, PhuxDocumentPoint, PhuxResourceId, PhuxTerminalGridView, RenderCache,
    ResourceId, bytes_out, engine_bridge, grid, ptr, resource_id_with_host, terminal_id_out,
};
#[cfg(test)]
use super::{EFFECT_VIEW_BUILDS, RENDER_CACHE_BUILDS};

impl Client {
    pub(crate) const fn publish_effects(&mut self) {
        self.effect_count = self.owned_effects.len();
    }

    pub(crate) fn effect_view(&self, index: usize) -> Option<PhuxClientEffect> {
        self.owned_effects[..self.effect_count]
            .get(index)
            .map(|effect| {
                #[cfg(test)]
                EFFECT_VIEW_BUILDS.set(EFFECT_VIEW_BUILDS.get() + 1);
                PhuxClientEffect {
                    kind: effect.kind,
                    detail: effect.detail,
                    status_code: effect.status_code,
                    terminal_id: terminal_id_out(&effect.terminal_id),
                    stream_id: effect.stream_id,
                    bootstrap_id: effect.bootstrap_id,
                    seq: effect.seq,
                    first_row: effect.first_row,
                    last_row: effect.last_row,
                    bytes: bytes_out(&effect.bytes),
                }
            })
    }

    pub(crate) fn build_grid(
        &mut self,
        id: &ResourceId,
    ) -> Result<*const PhuxTerminalGridView, BridgeError> {
        let frame = self
            .control()
            .publication()
            .acquire(id)
            .ok_or_else(|| BridgeError::state("terminal has no published grid"))?;
        let info = self.engine()?.replica_info(id).map_err(engine_bridge)?;
        let history = history_counters(info.history.as_ref());
        if self
            .render
            .get(id)
            .is_some_and(|cache| render_cache_matches(cache, &frame, &info, &history))
        {
            let top_anchor = self.track_anchor(
                id,
                PhuxDocumentPoint {
                    space: 1,
                    row: 0,
                    column: 0,
                    reserved: 0,
                },
            )?;
            let Some(cache) = self.render.get_mut(id) else {
                return Err(BridgeError::state("render cache disappeared"));
            };
            cache.view.top_anchor = top_anchor;
            cache.metadata.valid = true;
            return Ok(ptr::from_ref(&cache.view));
        }
        #[cfg(test)]
        RENDER_CACHE_BUILDS.set(RENDER_CACHE_BUILDS.get() + 1);
        let top_anchor = self.track_anchor(
            id,
            PhuxDocumentPoint {
                space: 1,
                row: 0,
                column: 0,
                reserved: 0,
            },
        )?;
        let mut host = Vec::new();
        let terminal_id = view_terminal_id(id, &mut host);
        // One read of the frame, shared with the UniFFI lane; this one is
        // lent by pointer into the `Arc<GridFrame>` the cache retains.
        let projected = grid::view(&frame);
        let view = PhuxTerminalGridView {
            terminal_id,
            stream_id: projected.stream_id,
            bootstrap_id: projected.bootstrap_id,
            last_seq: projected.last_seq,
            document_revision: info.document_revision,
            cols: projected.cols,
            rows: projected.rows,
            cells: if projected.cells.is_empty() {
                ptr::null()
            } else {
                projected.cells.as_ptr()
            },
            cell_count: projected.cells.len(),
            utf8: bytes_out(projected.utf8),
            cursor_visible: projected.cursor.visible,
            cursor_col: projected.cursor.col,
            cursor_row: projected.cursor.row,
            cursor_style: projected.cursor.style as u32,
            history_total_rows: projected.scrollbar.total,
            history_viewport_offset: projected.scrollbar.offset,
            history_visible_rows: projected.scrollbar.len,
            history_pages_loaded: history.pages,
            history_unread_rows: history.unread_rows,
            history_bytes_loaded: history.bytes,
            history_loading: history.loading,
            history_has_more: history.has_more,
            top_anchor,
        };
        let metadata = GridMetadataCache::from_frame(&frame, info.document_revision);
        self.render.insert(
            id.clone(),
            RenderCache {
                frame,
                _terminal_host: host,
                view,
                metadata,
                draw: None,
            },
        );
        let Some(cache) = self.render.get(id) else {
            return Err(BridgeError::state("render cache insertion failed"));
        };
        Ok(ptr::from_ref(&cache.view))
    }

    pub(crate) fn perf_json(&mut self) {
        let mut report = phux_client_core::perf::report(self.created_at.elapsed());
        report.metrics.extend(phux_client_runtime::perf::snapshot());
        self.perf_buf = report.to_json().into_bytes();
    }
}
fn render_cache_matches(
    cache: &RenderCache,
    frame: &Arc<GridFrame>,
    info: &phux_client_runtime::engine::ReplicaInfo,
    history: &HistoryCounters,
) -> bool {
    Arc::ptr_eq(&cache.frame, frame)
        && cache.view.document_revision == info.document_revision
        && cache.view.history_pages_loaded == history.pages
        && cache.view.history_unread_rows == history.unread_rows
        && cache.view.history_bytes_loaded == history.bytes
        && cache.view.history_loading == history.loading
        && cache.view.history_has_more == history.has_more
}

fn view_terminal_id(id: &ResourceId, host: &mut Vec<u8>) -> PhuxResourceId {
    if let ResourceId::Satellite {
        host: satellite, ..
    } = id
    {
        host.extend_from_slice(satellite.as_str().as_bytes());
    }
    resource_id_with_host(id, host)
}

struct HistoryCounters {
    pages: u64,
    bytes: u64,
    unread_rows: u64,
    loading: bool,
    has_more: bool,
}

fn history_counters(status: Option<&HistoryStatus>) -> HistoryCounters {
    status.map_or(
        HistoryCounters {
            pages: 0,
            bytes: 0,
            unread_rows: 0,
            loading: false,
            has_more: false,
        },
        |status| HistoryCounters {
            pages: status.loaded_pages as u64,
            bytes: status.loaded_bytes as u64,
            unread_rows: status.unread_rows,
            loading: status.state == HistoryLoadState::Loading,
            has_more: status.next_cursor.is_some(),
        },
    )
}
