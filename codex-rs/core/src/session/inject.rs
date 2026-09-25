use std::borrow::Borrow;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::TurnInput as PendingTurnInput;
use super::session::Session;
use super::turn_context::TurnContext;
use crate::state::TurnState;
use codex_analytics::ImagePreparationMetadata;
use codex_features::Feature;
use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;

pub(crate) const USER_ROLE_RESPONSE_ITEM_ERROR: &str =
    "user-role response items cannot be injected; submit direct user input through the turn API";

pub(crate) fn validate_live_response_items<T: Borrow<ResponseItem>>(
    items: &[T],
) -> CodexResult<()> {
    if items
        .iter()
        .any(|item| matches!(item.borrow(), ResponseItem::Message { role, .. } if role == "user"))
    {
        return Err(CodexErr::InvalidRequest(
            USER_ROLE_RESPONSE_ITEM_ERROR.to_string(),
        ));
    }
    Ok(())
}

impl Session {
    /// Returns the input when it cannot be injected because the thread has no active turn or the
    /// input contains a forbidden user-role response item.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub(crate) async fn inject_if_running<T: Into<ResponseItemEnvelope> + Borrow<ResponseItem>>(
        &self,
        input: Vec<T>,
    ) -> Result<(), Vec<T>> {
        if validate_live_response_items(&input).is_err() {
            return Err(input);
        }
        let mut active = self.active_turn.lock().await;
        match active.as_mut() {
            Some(active_turn) => {
                self.input_queue
                    .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                        active_turn.turn_state.as_ref(),
                        input
                            .into_iter()
                            .map(Into::into)
                            .map(PendingTurnInput::ResponseItem)
                            .collect(),
                    )
                    .await;
                Ok(())
            }
            None => Err(input),
        }
    }

    /// Injects hook context into the running turn atomically.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn provenance and turn state updates must remain atomic"
    )]
    pub(crate) async fn inject_hook_context_if_running(
        &self,
        input: Vec<ResponseItem>,
    ) -> Result<(), Vec<ResponseItem>> {
        let mut active = self.active_turn.lock().await;
        let Some(active_turn) = active.as_mut() else {
            return Err(input);
        };
        if active_turn.task.is_none() {
            return Err(input);
        }
        self.input_queue
            .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                active_turn.turn_state.as_ref(),
                input
                    .into_iter()
                    .map(ResponseItemEnvelope::new)
                    .map(PendingTurnInput::ResponseItem)
                    .collect(),
            )
            .await;
        Ok(())
    }

    /// Preserves trusted client provenance while items wait for an active turn.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub(crate) async fn inject_client_response_items(
        &self,
        items: Vec<ResponseItem>,
        turn_context: &TurnContext,
    ) {
        let items = items
            .into_iter()
            .map(|item| self.annotate_client_response_item(item))
            .collect::<Vec<_>>();
        let mut active = self.active_turn.lock().await;
        if let Some(active_turn) = active.as_mut() {
            self.input_queue
                .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                    active_turn.turn_state.as_ref(),
                    items
                        .into_iter()
                        .map(PendingTurnInput::ResponseItem)
                        .collect(),
                )
                .await;
            return;
        }
        drop(active);
        self.record_annotated_conversation_items(turn_context, turn_context.model_info(), items)
            .await;
    }

    /// Releases a finished turn's state so that no input queued on it is lost.
    ///
    /// Invariant, shared with [`Self::inject_client_response_items`]: input queued on a turn
    /// state under the `active_turn` lock is consumed by that turn, moved to the active turn
    /// that replaced it, or returned to the caller to record. The caller must first drain the
    /// state and detach its task.
    ///
    /// Returns whether this call cleared the active turn, and late input the caller must record.
    /// - This state is active with a task: a new task reuses it and owns its input.
    /// - This state is active without a task: clear the active turn and return its input.
    /// - Another turn replaced it: move its input to that turn, as a later inject would.
    /// - No active turn: return its input, as an idle inject would record it.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub(crate) async fn release_finished_turn_state(
        &self,
        turn_state: &Arc<Mutex<TurnState>>,
    ) -> (bool, Vec<PendingTurnInput>) {
        let mut active = self.active_turn.lock().await;
        let mut cleared_active_turn = false;
        if let Some(active_turn) = active.as_ref()
            && Arc::ptr_eq(&active_turn.turn_state, turn_state)
        {
            if active_turn.task.is_some() {
                return (false, Vec::new());
            }
            *active = None;
            cleared_active_turn = true;
        }
        let late_input = self
            .input_queue
            .take_pending_input_for_turn_state(turn_state.as_ref())
            .await;
        match active.as_ref() {
            Some(successor) => {
                if !late_input.is_empty() {
                    self.input_queue
                        .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                            successor.turn_state.as_ref(),
                            late_input,
                        )
                        .await;
                }
                (false, Vec::new())
            }
            None => (cleared_active_turn, late_input),
        }
    }

    pub(crate) fn annotate_client_response_item(&self, item: ResponseItem) -> ResponseItemEnvelope {
        let metadata = (self.enabled(Feature::RetainClientDeveloperMessages)
            && matches!(&item, ResponseItem::Message { role, .. } if role == "developer"))
        .then_some(CodexHarnessMetadata {
            client_authored: true,
            ..Default::default()
        });

        ResponseItemEnvelope { item, metadata }
    }

    pub(crate) async fn record_annotated_conversation_items(
        &self,
        turn_context: &TurnContext,
        model_info: &ModelInfo,
        items: Vec<ResponseItemEnvelope>,
    ) {
        if items.iter().all(|item| item.metadata.is_none()) {
            let items = items
                .into_iter()
                .map(ResponseItemEnvelope::into_item)
                .collect::<Vec<_>>();
            self.record_conversation_items(turn_context, model_info, &items)
                .await;
            return;
        }

        let (annotated_items, image_preparations, _) = self
            .prepare_annotated_conversation_items_for_history(turn_context, model_info, items)
            .await;
        self.record_prepared_conversation_items(
            turn_context,
            model_info,
            annotated_items,
            image_preparations,
        )
        .await;
    }

    /// Also returns the original content indices that failed media removed from the first item.
    pub(super) async fn prepare_annotated_conversation_items_for_history(
        &self,
        turn_context: &TurnContext,
        model_info: &ModelInfo,
        items: Vec<ResponseItemEnvelope>,
    ) -> (
        Vec<ResponseItemEnvelope>,
        Vec<ImagePreparationMetadata>,
        Vec<usize>,
    ) {
        let mut annotated_items = Vec::with_capacity(items.len());
        let mut image_preparations = Vec::new();
        let mut first_removed_content_indices = None;
        for envelope in items {
            let (prepared_items, prepared_images, removed_content_indices) = self
                .prepare_conversation_items_for_history_with_removed_user_content(
                    turn_context,
                    model_info,
                    std::slice::from_ref(&envelope.item),
                )
                .await;
            image_preparations.extend(prepared_images);
            first_removed_content_indices.get_or_insert(removed_content_indices);

            let mut metadata = envelope.metadata;
            annotated_items.extend(prepared_items.into_owned().into_iter().map(|item| {
                ResponseItemEnvelope {
                    item,
                    metadata: metadata.take(),
                }
            }));
        }
        (
            annotated_items,
            image_preparations,
            first_removed_content_indices.unwrap_or_default(),
        )
    }

    /// Injects items into active work, or records them without starting a turn.
    pub(crate) async fn inject_no_new_turn(
        &self,
        items: Vec<ResponseItem>,
        current_turn_context: Option<&TurnContext>,
    ) {
        if validate_live_response_items(&items).is_err() {
            return;
        }
        let Err(items) = self.inject_if_running(items).await else {
            return;
        };
        let default_turn_context;
        let turn_context = match current_turn_context {
            Some(turn_context) => turn_context,
            None => {
                default_turn_context = self.new_default_turn().await;
                default_turn_context.as_ref()
            }
        };
        self.record_conversation_items(turn_context, turn_context.model_info(), &items)
            .await;
    }
}

#[cfg(test)]
#[path = "inject_tests.rs"]
mod tests;
