use super::CodexThread;

impl CodexThread {
    /// Includes an idle-start reservation, not only an executing task.
    pub async fn has_active_turn(&self) -> bool {
        self.session.active_turn.lock().await.is_some()
    }

    /// Revoke prior local intent before persisting validated queued human input.
    /// This does not submit a turn, change its settings, or consume the queue.
    pub async fn notify_queued_user_input(&self) {
        let active = self.session.active_turn.lock().await;
        let turn_id = active.as_ref().and_then(|turn| {
            turn.task
                .as_ref()
                .map(|task| task.turn_context.sub_id.as_str())
        });
        self.session.notify_user_input(turn_id);
    }
}
