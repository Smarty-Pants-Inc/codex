use super::Session;
use codex_protocol::dynamic_tools::DynamicToolResponse;
use tracing::warn;

pub(super) enum DynamicToolResponseTarget<'a> {
    ActiveTurn,
    Turn(&'a str),
}

impl Session {
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "turn identity and pending call removal must remain atomic"
    )]
    pub(super) async fn notify_dynamic_tool_response(
        &self,
        target: DynamicToolResponseTarget<'_>,
        call_id: &str,
        response: DynamicToolResponse,
    ) {
        let entry = {
            let mut active = self.active_turn.lock().await;
            match active.as_mut() {
                Some(turn) => {
                    if let DynamicToolResponseTarget::Turn(expected) = target
                        && turn
                            .task
                            .as_ref()
                            .is_none_or(|task| task.turn_context.sub_id != expected)
                    {
                        return;
                    }
                    turn.turn_state
                        .lock()
                        .await
                        .remove_pending_dynamic_tool(call_id)
                }
                None => None,
            }
        };
        match entry {
            Some(sender) => {
                // Removal commits delivery to this turn. A later interrupt owns
                // cancellation; it must not redirect the accepted response.
                sender.send(response).ok();
            }
            None => warn!("No pending dynamic tool call found for call_id: {call_id}"),
        }
    }
}
