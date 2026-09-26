//! Stamps which realtime input, voice or typed, each assistant item responds to.
//!
//! Only turns that accepted a realtime delegation track an origin, so other
//! turns keep `None`. The origin is event metadata only and never reaches
//! model-visible context.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;

use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::ItemOrigin;
use codex_protocol::items::ReasoningItem;
use codex_protocol::items::TurnItem;
use codex_protocol::user_input::UserInput;

use super::TurnContext;
use super::TurnInput;
use crate::context::ContextualUserFragment;
use crate::context::RealtimeDelegation;

/// Exists only after the turn accepted voice input, so `current` is then always set.
#[derive(Default)]
struct TurnItemOrigins {
    current: Option<ItemOrigin>,
    started: HashMap<String, ItemOrigin>,
}

#[derive(Default)]
struct ItemOriginState(Mutex<TurnItemOrigins>);

/// Updates the current origin for one accepted (not hook-blocked) input item.
pub(super) fn note_accepted_input(turn_context: &TurnContext, input: &TurnInput) {
    match input {
        TurnInput::DeveloperInput { content }
            if matches!(
                content.as_slice(),
                [UserInput::Text { text, .. }] if RealtimeDelegation::matches_text(text)
            ) =>
        {
            let state = turn_context
                .extension_data
                .get_or_init(ItemOriginState::default);
            lock(&state).current = Some(ItemOrigin::Voice);
        }
        TurnInput::UserInput { content, .. } if !content.is_empty() => {
            if let Some(state) = turn_context.extension_data.get::<ItemOriginState>() {
                lock(&state).current = Some(ItemOrigin::Typed);
            }
        }
        TurnInput::UserInput { .. }
        | TurnInput::DeveloperInput { .. }
        | TurnInput::FunctionCallOutput(_)
        | TurnInput::ResponseItem(_)
        | TurnInput::InterAgentCommunication(_) => {}
    }
}

/// Records and sets the current origin on an assistant item as it starts.
pub(super) fn stamp_started(turn_context: &TurnContext, mut item: TurnItem) -> TurnItem {
    if let Some(state) = turn_context.extension_data.get::<ItemOriginState>()
        && let Some((id, origin)) = origin_slot(&mut item)
    {
        let mut origins = lock(&state);
        *origin = origins.current;
        if let Some(current) = origins.current {
            origins.started.insert(id.to_string(), current);
        }
    }
    item
}

/// Sets the origin recorded at start, or the current origin for completion-only items.
pub(super) fn stamp_completed(turn_context: &TurnContext, mut item: TurnItem) -> TurnItem {
    if let Some(state) = turn_context.extension_data.get::<ItemOriginState>()
        && let Some((id, origin)) = origin_slot(&mut item)
    {
        let mut origins = lock(&state);
        let started = origins.started.remove(id);
        *origin = started.or(origins.current);
    }
    item
}

fn lock(state: &ItemOriginState) -> MutexGuard<'_, TurnItemOrigins> {
    state.0.lock().unwrap_or_else(PoisonError::into_inner)
}

fn origin_slot(item: &mut TurnItem) -> Option<(&str, &mut Option<ItemOrigin>)> {
    match item {
        TurnItem::AgentMessage(AgentMessageItem { id, origin, .. })
        | TurnItem::Reasoning(ReasoningItem { id, origin, .. }) => Some((id.as_str(), origin)),
        TurnItem::UserMessage(_)
        | TurnItem::FunctionCallOutput(_)
        | TurnItem::HookPrompt(_)
        | TurnItem::Plan(_)
        | TurnItem::CommandExecution(_)
        | TurnItem::DynamicToolCall(_)
        | TurnItem::CollabAgentToolCall(_)
        | TurnItem::SubAgentActivity(_)
        | TurnItem::WebSearch(_)
        | TurnItem::ImageView(_)
        | TurnItem::Extension(_)
        | TurnItem::ImageGeneration(_)
        | TurnItem::EnteredReviewMode(_)
        | TurnItem::ExitedReviewMode(_)
        | TurnItem::FileChange(_)
        | TurnItem::McpToolCall(_)
        | TurnItem::ContextCompaction(_) => None,
    }
}
