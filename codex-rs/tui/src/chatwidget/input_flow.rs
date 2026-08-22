//! User input submission, queue draining, and draft restore flow for `ChatWidget`.
//!
//! The queue data itself lives in `input_queue`; this module owns the app-level
//! effects around taking composer input, submitting user turns, draining queued
//! follow-ups, and restoring draft state across interrupts or thread switches.

use std::collections::HashSet;
use std::collections::VecDeque;

use super::*;

impl ChatWidget {
    pub(crate) fn set_parent_owned_thread(&mut self) {
        self.blocks_direct_input = true;
        self.bottom_pane.set_parent_owned_thread();
    }

    pub(super) fn handle_composer_input_result(
        &mut self,
        input_result: InputResult,
        had_modal_or_popup: bool,
    ) {
        match input_result {
            InputResult::Submitted {
                text,
                text_elements,
            } => {
                let user_message = self.user_message_from_submission(text, text_elements);
                if user_message.text.is_empty()
                    && user_message.local_images.is_empty()
                    && user_message.remote_image_urls.is_empty()
                {
                    return;
                }
                let should_submit_now = self.is_session_configured()
                    && !self.is_plan_streaming_in_tui()
                    && !self.input_queue.suppress_queue_autosend
                    && (!self.input_queue.user_turn_pending_start
                        || self.turn_lifecycle.agent_turn_running);
                if should_submit_now {
                    if self.only_user_shell_commands_running()
                        && !user_message.text.starts_with('!')
                    {
                        self.queue_user_message(user_message);
                        return;
                    }
                    // Submitted is emitted when user submits.
                    // Reset any reasoning header only when we are actually submitting a turn.
                    self.reasoning_buffer.clear();
                    self.reasoning_header = None;
                    self.reasoning_summary_parts.clear();
                    self.set_status_header(String::from("Working"));
                    self.submit_user_message(user_message);
                } else {
                    self.queue_user_message(user_message);
                }
            }
            InputResult::Queued {
                text,
                text_elements,
                action,
                pending_pastes,
            } => {
                let user_message = self.user_message_from_submission(text, text_elements);
                self.queue_user_message_with_options(user_message, action, pending_pastes);
            }
            InputResult::Command(cmd) => {
                self.handle_slash_command_dispatch(cmd);
            }
            InputResult::ServiceTierCommand(command) => {
                self.handle_service_tier_command_dispatch(command);
            }
            InputResult::CommandWithArgs(cmd, args, text_elements) => {
                self.handle_slash_command_with_args_dispatch(cmd, args, text_elements);
            }
            InputResult::ParentOwnedInputBlocked => {
                self.add_error_message(PARENT_OWNED_INPUT_MESSAGE.to_string());
            }
            InputResult::None => {}
        }
        if had_modal_or_popup && self.bottom_pane.no_modal_or_popup_active() {
            self.maybe_send_next_queued_input();
        }
    }

    pub(super) fn defer_input_until_settings_applied(&mut self) {
        if !self.bottom_pane.no_modal_or_popup_active() {
            self.input_queue.suppress_queue_autosend = true;
        }
    }

    pub(super) fn on_modal_or_popup_closed(&mut self) {
        if self.input_queue.suppress_queue_autosend {
            self.app_event_tx.send(AppEvent::SettingsSelectionClosed);
        } else {
            self.maybe_send_next_queued_input();
        }
    }

    pub(super) fn queue_user_message(&mut self, user_message: UserMessage) {
        self.queue_user_message_with_options(user_message, QueuedInputAction::Plain, Vec::new());
    }

    pub(crate) fn set_queue_submissions_until_session_configured(&mut self, queue: bool) {
        self.bottom_pane
            .set_queue_submissions(queue && !self.is_session_configured());
    }

    pub(super) fn queue_user_message_with_options(
        &mut self,
        user_message: UserMessage,
        action: QueuedInputAction,
        pending_pastes: Vec<(String, String)>,
    ) {
        if self.misalignment_policy_violation {
            return;
        }
        let should_run_now = self.is_session_configured()
            && !self.is_user_turn_pending_or_running()
            && !self.input_queue.suppress_queue_autosend;
        let can_admit_to_server_queue = matches!(&self.codex_op_target, CodexOpTarget::AppEvent)
            && action == QueuedInputAction::Plain
            && self.is_session_configured()
            && self.is_user_turn_pending_or_running()
            && self.thread_id.is_some()
            && self.input_queue.can_admit_to_server_queue()
            && (user_message.local_images.is_empty() && user_message.remote_image_urls.is_empty()
                || self.current_model_supports_images())
            && !self
                .effective_collaboration_mode()
                .model()
                .trim()
                .is_empty();
        let server_queue_request = can_admit_to_server_queue.then(|| {
            let mut input = self.app_server_user_inputs(&user_message);
            self.maybe_apply_ide_context(&mut input);
            (
                self.thread_id
                    .expect("configured queued message must have a thread"),
                uuid::Uuid::new_v4().to_string(),
                input,
            )
        });
        if !should_run_now || action != QueuedInputAction::Plain {
            let mut queued_message = QueuedUserMessage::new(user_message, action);
            queued_message.pending_pastes = pending_pastes;
            if let Some((_, client_user_message_id, _)) = &server_queue_request {
                queued_message.mark_pending_server_admission(client_user_message_id.clone());
            }
            self.input_queue
                .queued_user_messages
                .push_back(queued_message);
            self.input_queue
                .queued_user_message_history_records
                .push_back(UserMessageHistoryRecord::UserMessageText);
            self.refresh_pending_input_preview();
            if let Some((thread_id, client_user_message_id, input)) = server_queue_request {
                self.app_event_tx.send(AppEvent::QueueFollowUpUserMessage {
                    thread_id,
                    client_user_message_id,
                    input,
                });
            }
            if should_run_now {
                self.maybe_send_next_queued_input();
            }
        } else {
            self.submit_user_message(user_message);
        }
    }

    pub(crate) fn has_pending_server_queue_admission(&self, client_user_message_id: &str) -> bool {
        self.input_queue
            .queued_user_messages
            .iter()
            .any(|message| message.pending_server_admission_id() == Some(client_user_message_id))
    }

    pub(crate) fn mark_server_queue_admitted(
        &mut self,
        client_user_message_id: &str,
        queued_submission_id: String,
    ) -> bool {
        let admitted = self
            .input_queue
            .queued_user_messages
            .iter_mut()
            .find(|message| message.pending_server_admission_id() == Some(client_user_message_id))
            .is_some_and(|message| {
                message.mark_server_queued(client_user_message_id, queued_submission_id)
            });
        if admitted {
            self.refresh_pending_input_preview();
            self.request_redraw();
        }
        admitted
    }

    pub(crate) fn reject_server_queue_admission(&mut self, client_user_message_id: &str) -> bool {
        let Some(first_rejected_index) =
            self.input_queue
                .queued_user_messages
                .iter()
                .position(|message| {
                    message.pending_server_admission_id() == Some(client_user_message_id)
                })
        else {
            return false;
        };
        for message in self
            .input_queue
            .queued_user_messages
            .iter_mut()
            .skip(first_rejected_index)
        {
            message.clear_pending_server_admission();
        }
        self.refresh_pending_input_preview();
        self.request_redraw();
        true
    }

    pub(crate) fn reconcile_server_queued_follow_ups(
        &mut self,
        thread_id: ThreadId,
        queued_submission_ids: Vec<String>,
    ) {
        if self.thread_id != Some(thread_id) {
            return;
        }
        let queued_submission_ids = queued_submission_ids.into_iter().collect::<HashSet<_>>();
        let queued_messages = std::mem::take(&mut self.input_queue.queued_user_messages);
        let mut history_records =
            std::mem::take(&mut self.input_queue.queued_user_message_history_records);
        let mut retained_messages = VecDeque::new();
        let mut retained_history_records = VecDeque::new();
        let mut removed_any = false;
        for message in queued_messages {
            let history_record = history_records
                .pop_front()
                .unwrap_or(UserMessageHistoryRecord::UserMessageText);
            if message
                .queued_submission_id()
                .is_some_and(|id| !queued_submission_ids.contains(id))
            {
                removed_any = true;
            } else {
                retained_messages.push_back(message);
                retained_history_records.push_back(history_record);
            }
        }
        self.input_queue.queued_user_messages = retained_messages;
        self.input_queue.queued_user_message_history_records = retained_history_records;
        if removed_any {
            self.refresh_pending_input_preview();
            self.request_redraw();
        }
    }

    /// If idle and there are queued inputs, submit exactly one to start the next turn.
    pub(crate) fn maybe_send_next_queued_input(&mut self) -> bool {
        if !self.is_session_configured()
            || self.misalignment_policy_violation
            || self.input_queue.suppress_queue_autosend
        {
            return false;
        }
        if self.blocks_direct_input {
            return false;
        }
        if self.is_user_turn_pending_or_running() {
            return false;
        }
        let mut submitted_follow_up = false;
        while !self.is_user_turn_pending_or_running() {
            if self.input_queue.rejected_steers_queue.is_empty()
                && self
                    .input_queue
                    .queued_user_messages
                    .front()
                    .is_some_and(QueuedUserMessage::is_server_managed)
            {
                submitted_follow_up = true;
                break;
            }
            let Some((queued_message, history_record)) = self.pop_next_queued_user_message() else {
                break;
            };
            match queued_message.action {
                QueuedInputAction::Plain => {
                    submitted_follow_up = self.submit_user_message_with_history_record(
                        queued_message.into_user_message(),
                        history_record,
                    );
                    break;
                }
                QueuedInputAction::Literal => {
                    let QueuedUserMessage {
                        user_message,
                        pending_pastes,
                        ..
                    } = queued_message;
                    let mut restored_pending_pastes = self.bottom_pane.composer_pending_pastes();
                    let mut used_paste_placeholders = restored_pending_pastes
                        .iter()
                        .map(|(placeholder, _)| placeholder.clone())
                        .collect();
                    let (mut user_message, pending_pastes) =
                        super::user_messages::remap_colliding_paste_placeholders(
                            user_message,
                            pending_pastes,
                            &mut used_paste_placeholders,
                        );
                    if !self.current_model().trim().is_empty()
                        && (self.current_model_supports_images()
                            || (user_message.local_images.is_empty()
                                && user_message.remote_image_urls.is_empty()))
                    {
                        (user_message.text, user_message.text_elements) =
                            crate::bottom_pane::ChatComposer::expand_pending_pastes(
                                &user_message.text,
                                user_message.text_elements,
                                &pending_pastes,
                            );
                    }
                    submitted_follow_up = self
                        .submit_user_message_with_shell_escape_policy(
                            user_message,
                            ShellEscapePolicy::Disallow,
                        )
                        .is_some();
                    if !submitted_follow_up {
                        restored_pending_pastes.extend(pending_pastes);
                        self.bottom_pane
                            .set_composer_pending_pastes(restored_pending_pastes);
                    }
                    break;
                }
                QueuedInputAction::ParseSlash => {
                    let drain = self.submit_queued_slash_prompt(queued_message);
                    if drain == QueueDrain::Stop {
                        submitted_follow_up = self.is_user_turn_pending_or_running();
                        break;
                    }
                }
                QueuedInputAction::RunShell => {
                    let drain = self.submit_queued_shell_prompt(queued_message.into_user_message());
                    if drain == QueueDrain::Stop {
                        submitted_follow_up = self.is_user_turn_pending_or_running();
                        break;
                    }
                }
            }
        }
        // Update the list to reflect the remaining queued messages (if any).
        self.refresh_pending_input_preview();
        submitted_follow_up
    }

    pub(crate) fn is_user_turn_pending_or_running(&self) -> bool {
        self.input_queue.user_turn_pending_start
            || self.turn_lifecycle.agent_turn_running
            || self.review.is_review_mode
            || (self.bottom_pane.is_task_running() && self.mcp_startup_status.is_none())
    }

    pub(super) fn only_user_shell_commands_running(&self) -> bool {
        self.turn_lifecycle.agent_turn_running
            && !self.running_commands.is_empty()
            && self
                .running_commands
                .values()
                .all(|command| command.source == ExecCommandSource::UserShell)
    }

    /// Rebuild and update the bottom-pane pending-input preview.
    pub(super) fn refresh_pending_input_preview(&mut self) {
        let preview = self.input_queue.preview();
        self.bottom_pane.set_pending_input_preview(
            preview.queued_messages,
            preview.pending_steers,
            preview.rejected_steers,
        );
    }

    pub(crate) fn submit_user_message_with_mode(
        &mut self,
        text: String,
        mut collaboration_mode: CollaborationModeMask,
    ) {
        if self.blocks_direct_input {
            self.add_error_message(PARENT_OWNED_INPUT_MESSAGE.to_string());
            return;
        }
        if collaboration_mode.mode == Some(ModeKind::Plan)
            && let Some(effort) = self.config.plan_mode_reasoning_effort.clone()
        {
            collaboration_mode.reasoning_effort = Some(Some(effort));
        }
        if self.turn_lifecycle.agent_turn_running
            && self.active_collaboration_mask.as_ref() != Some(&collaboration_mode)
        {
            self.add_error_message(
                "Cannot switch collaboration mode while a turn is running.".to_string(),
            );
            return;
        }
        self.set_collaboration_mask_from_user_action(collaboration_mode);
        let should_queue = self.is_plan_streaming_in_tui();
        let user_message = UserMessage {
            text,
            local_images: Vec::new(),
            remote_image_urls: Vec::new(),
            text_elements: Vec::new(),
            mention_bindings: Vec::new(),
        };
        if should_queue {
            self.queue_user_message(user_message);
        } else {
            self.submit_user_message(user_message);
        }
    }

    #[cfg(test)]
    pub(crate) fn queued_user_message_texts(&self) -> Vec<String> {
        self.input_queue
            .rejected_steers_queue
            .iter()
            .map(|message| message.text.clone())
            .chain(
                self.input_queue
                    .queued_user_messages
                    .iter()
                    .map(|message| message.text.clone()),
            )
            .collect()
    }
}
