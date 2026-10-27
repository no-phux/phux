//! Owner-thread query dispatch.

#[cfg(feature = "engine")]
use super::{GhosttyReplica, ReplicaPrompt, engine_error, views};
use super::{InputEligibility, Owner, Query};

impl Owner {
    pub(super) fn query(&mut self, query: Query) {
        let Some(query) = self.query_common(query) else {
            return;
        };
        #[cfg(feature = "engine")]
        {
            let Some(query) = self.query_presentation(query) else {
                return;
            };
            self.query_document(query);
        }
        #[cfg(not(feature = "engine"))]
        if let Query::TakeOutput(id, reply) = query {
            let bytes = if self.visible.contains(&id) {
                self.kernel
                    .published_engine_mut(&id)
                    .map(|replica| std::mem::take(&mut replica.bytes))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let _ = reply.send(bytes);
        }
    }

    fn query_common(&self, query: Query) -> Option<Query> {
        match query {
            Query::HasProjection(id, reply) => {
                let _ = reply.send(self.has_replica(&id));
            }
            Query::IsClosed(id, reply) => {
                let _ = reply.send(self.is_closed(&id));
            }
            Query::AgentStreamInfo(id, reply) => {
                let _ = reply.send(self.kernel.agent_stream_info(&id));
            }
            Query::HasNativeAgentView(id, reply) => {
                let _ = reply.send(self.kernel.has_native_agent_view(&id));
            }
            Query::NativeAgentView(id, reply) => {
                let _ = reply.send(self.kernel.native_agent_view(&id));
            }
            Query::InputEligibility(id, reply) => {
                let _ = reply.send(self.kernel.input_eligibility(&id));
            }
            Query::InputReady(id, reply) => {
                let ready = matches!(
                    self.kernel.input_eligibility(&id),
                    InputEligibility::Eligible { .. }
                );
                let _ = reply.send(ready);
            }
            #[cfg(not(feature = "engine"))]
            query @ Query::TakeOutput(..) => return Some(query),
            #[cfg(feature = "engine")]
            query => return Some(query),
        }
        None
    }

    #[cfg(feature = "engine")]
    fn query_presentation(&mut self, query: Query) -> Option<Query> {
        match query {
            Query::Scroll(id, scroll, reply) => {
                let _ = reply.send(self.scroll(&id, scroll));
            }
            Query::IsAltScreen(id, reply) => {
                let alt = self
                    .replica(&id)
                    .and_then(GhosttyReplica::terminal)
                    .is_some_and(|terminal| {
                        matches!(
                            terminal.active_screen(),
                            Ok(libghostty_vt::screen::Screen::Alternate)
                        )
                    });
                let _ = reply.send(alt);
            }
            Query::PredictText(id, text, reply) => {
                let result = self
                    .predict_text(&id, &text)
                    .and_then(|()| self.render_and_publish(&id));
                let _ = reply.send(result);
            }
            Query::ClearPredictions(id, reply) => {
                if let Some(slot) = self.presentations.get_mut(&id) {
                    slot.predictor.clear();
                }
                let _ = reply.send(self.render_and_publish(&id));
            }
            Query::CellPixels(id, width, height, reply) => {
                let changed =
                    self.cell_pixels.insert(id.clone(), (width, height)) != Some((width, height));
                let result = self
                    .kernel
                    .published_engine_mut(&id)
                    .ok_or_else(|| engine_error("terminal has no published replica"))
                    .and_then(|replica| {
                        if changed {
                            replica
                                .set_cell_pixels(width, height)
                                .map_err(|error| engine_error(error.to_string()))
                        } else {
                            Ok(())
                        }
                    })
                    .and_then(|()| {
                        if changed {
                            self.render_views(&id).map(|_| ())
                        } else {
                            Ok(())
                        }
                    });
                // Exists only to keep desktop-perf-budget's buffer_fresh count
                // stable after a pitch republish. The real fix is to not
                // republish render_views before any consumer has acquired a
                // frame, then drop this. Tracked in PHUX-557.
                if changed {
                    self.discard_unheld_retired(&id);
                }
                let _ = reply.send(result);
            }
            Query::Republish(id, reply) => {
                let _ = reply.send(self.render_and_publish(&id));
            }
            Query::ReplicaInfo(id, reply) => {
                let _ = reply.send(self.replica_info(&id));
            }
            Query::Present(id, reply) => {
                let _ = reply.send(self.present(&id));
            }
            Query::MouseMode(id, reply) => {
                let _ = reply.send(self.mouse_mode(&id));
            }
            Query::BracketedPaste(id, reply) => {
                let _ = reply.send(self.bracketed_paste(&id));
            }
            Query::Prompt(id, reply) => {
                let _ = reply.send(
                    self.terminal(&id)
                        .map_or(ReplicaPrompt::Unknown, views::replica_prompt),
                );
            }
            query => return Some(query),
        }
        None
    }

    #[cfg(feature = "engine")]
    fn query_document(&mut self, query: Query) {
        match query {
            Query::TrackAnchor(id, point, reply) => {
                let _ = reply.send(self.track_anchor(&id, point));
            }
            Query::ReleaseAnchor(id, anchor, reply) => {
                let _ = reply.send(self.release_anchor(&id, anchor));
            }
            Query::ClearPresentation(id, stream, bootstrap, reply) => {
                let _ = reply.send(self.clear_presentation(&id, stream, bootstrap));
            }
            Query::PinViewport(id, anchor, reply) => {
                let _ = reply.send(self.pin_viewport(&id, anchor));
            }
            Query::FollowLive(id, reply) => {
                let _ = reply.send(self.follow_live(&id));
            }
            Query::SetSelection(id, start, end, rectangle, reply) => {
                let _ = reply.send(self.set_selection(&id, start, end, rectangle));
            }
            Query::ClearSelection(id, reply) => {
                let _ = reply.send(self.clear_selection(&id));
            }
            Query::SelectionText(id, reply) => {
                let _ = reply.send(self.selection_text(&id));
            }
            Query::SelectionTextBounded(id, max_bytes, reply) => {
                let _ = reply.send(self.selection_text_bounded(&id, max_bytes));
            }
            Query::Search(id, query, case_sensitive, max_matches, reply) => {
                let _ = reply.send(self.search(&id, &query, case_sensitive, max_matches));
            }
            Query::ClearSearch(id, reply) => {
                let _ = reply.send(self.clear_search_handles(&id));
            }
            Query::Gesture(id, event, reply) => {
                let _ = reply.send(self.selection_gesture(&id, event));
            }
            Query::SelectAll(id, reply) => {
                let _ = reply.send(self.select_all(&id));
            }
            Query::RegionText(id, region, max_bytes, reply) => {
                let _ = reply.send(self.region_text(&id, region, max_bytes));
            }
            _ => {}
        }
    }
}
