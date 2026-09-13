//! Display canonical speech without starting local audio or submitting a model turn.

use super::*;
use codex_app_server_protocol::ThreadRealtimeItem;
use codex_app_server_protocol::ThreadRealtimeItemContent;
use codex_app_server_protocol::ThreadRealtimeTranscriptRole;
use std::collections::HashSet;
use std::collections::VecDeque;

// Match the app's bounded thread-event replay window. A rebuilt widget gets a fresh window.
const RECENT_TRANSCRIPT_CAPACITY: usize = 32_768;

#[derive(Default)]
pub(super) struct RealtimeTranscriptState {
    pub(super) history: crate::realtime_history::RealtimeHistory,
    seen: HashSet<(String, String)>,
    order: VecDeque<(String, String)>,
    pending: VecDeque<ThreadRealtimeItem>,
}

impl ChatWidget {
    pub(super) fn replay_realtime_history(
        &mut self,
        anchor: crate::realtime_history::RealtimeHistoryAnchor,
    ) {
        if let Some(items) = self.transcript.realtime.history.items.remove(&anchor)
            && let Some(thread_id) = self.thread_id()
        {
            for item in items {
                self.on_realtime_item_completed(&thread_id.to_string(), item);
            }
        }
    }

    pub(super) fn on_realtime_item_completed(&mut self, thread_id: &str, item: ThreadRealtimeItem) {
        if self
            .thread_id()
            .is_none_or(|id| id.to_string() != thread_id)
            || !matches!(&item.content, ThreadRealtimeItemContent::TranscriptSegment { text, .. } if !text.trim().is_empty())
        {
            return;
        }
        let state = &mut self.transcript.realtime;
        let key = (item.realtime_session_id.clone(), item.id.clone());
        if !state.seen.insert(key.clone()) {
            return;
        }
        state.order.push_back(key);
        if state.order.len() > RECENT_TRANSCRIPT_CAPACITY
            && let Some(oldest) = state.order.pop_front()
        {
            state.seen.remove(&oldest);
        }
        state.pending.push_back(item);
        self.flush_realtime_transcripts();
    }

    pub(super) fn flush_realtime_transcripts(&mut self) {
        // Do not split an ordinary assistant/plan stream or its queued consolidation.
        if self.stream_controller.is_some()
            || self.plan_stream_controller.is_some()
            || self.pending_stream_consolidations > 0
        {
            return;
        }
        while let Some(item) = self.transcript.realtime.pending.pop_front() {
            let ThreadRealtimeItemContent::TranscriptSegment { role, text } = item.content else {
                continue;
            };
            let cell: Box<dyn HistoryCell> = match role {
                ThreadRealtimeTranscriptRole::User => Box::new(history_cell::new_user_prompt(
                    text,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )),
                ThreadRealtimeTranscriptRole::Assistant => {
                    Box::new(
                        history_cell::AgentMarkdownCell::new_with_inline_visualizations(
                            text,
                            self.config.cwd.as_path(),
                            /*inline_visualization_context*/ None,
                        ),
                    )
                }
            };
            self.add_boxed_history(cell);
            self.request_redraw();
        }
    }
}
