//! Bounded canonical speech history, anchored to the existing ordinary replay.

use codex_app_server_protocol::ThreadRealtimeItem;
use codex_app_server_protocol::ThreadRealtimeItemContent;
use codex_app_server_protocol::ThreadTimelineEntry;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RealtimeHistoryAnchor {
    #[default]
    End,
    Item(String, String),
    Turn(String),
    AfterItem(String, String),
    AfterTurn(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RealtimeHistory {
    pub(crate) items: BTreeMap<RealtimeHistoryAnchor, Vec<ThreadRealtimeItem>>,
    pub(crate) notice: Option<String>,
}

impl RealtimeHistory {
    pub(crate) fn from_timeline(entries: Vec<ThreadTimelineEntry>) -> Self {
        let mut history = Self::default();
        let mut tail = RealtimeHistoryAnchor::End;
        let mut pending = Vec::new();
        // Anchor before the next ordinary entry so a page starting with speech
        // cannot move it ahead of older ordinary history outside this window.
        for entry in entries {
            let anchor = match entry {
                ThreadTimelineEntry::Item { turn_id, item, .. } => {
                    tail = RealtimeHistoryAnchor::AfterItem(turn_id.clone(), item.id().to_string());
                    RealtimeHistoryAnchor::Item(turn_id, item.id().to_string())
                }
                ThreadTimelineEntry::TurnCompleted { turn_id, .. } => {
                    tail = RealtimeHistoryAnchor::AfterTurn(turn_id.clone());
                    RealtimeHistoryAnchor::Turn(turn_id)
                }
                ThreadTimelineEntry::TurnStarted { .. } => continue,
                ThreadTimelineEntry::Realtime { item, .. } => {
                    if matches!(
                        &item.content,
                        ThreadRealtimeItemContent::TranscriptSegment { .. }
                    ) {
                        pending.push(item);
                    }
                    continue;
                }
            };
            if !pending.is_empty() {
                history
                    .items
                    .entry(anchor)
                    .or_default()
                    .append(&mut pending);
            }
        }
        if !pending.is_empty() {
            history.items.entry(tail).or_default().append(&mut pending);
        }
        history
    }
}
