use std::sync::Arc;

use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::tests::make_session_and_context_with_rx;
use crate::session::turn_context::TurnContext;
use crate::state::ActiveTurn;
use crate::state::TurnState;
use codex_features::Feature;
use codex_history::CodexHarnessMetadata;
use codex_history::ResponseItemEnvelope;
use codex_history::RolloutItem;
use codex_protocol::models::ConfigurationReasoning;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::EventMsg;
use pretty_assertions::assert_eq;
use tokio::sync::Mutex;

#[tokio::test]
async fn harness_authored_configuration_updates_preserve_metadata_and_resume() {
    let (session, turn_context, rx_event) = make_session_and_context_with_rx().await;
    assert!(!session.enabled(Feature::RetainClientDeveloperMessages));

    let mut expected = ResponseItemEnvelope {
        item: ResponseItem::ConfigurationUpdate {
            reasoning: ConfigurationReasoning {
                effort: ReasoningEffort::High,
            },
        },
        metadata: Some(CodexHarnessMetadata {
            harness_authored_configuration: true,
            ..Default::default()
        }),
    };
    session
        .record_annotated_conversation_items(
            &turn_context,
            turn_context.model_info(),
            vec![expected.clone()],
        )
        .await;

    expected.metadata.as_mut().unwrap().mcp_attribution = Some(
        session
            .services
            .executed_tool_calls
            .mcp_attribution_snapshot(),
    );

    let recorded = session.clone_history().await.into_annotated_items();
    assert_eq!(recorded, vec![expected.clone()]);
    let mut raw_items = Vec::new();
    while let Ok(event) = rx_event.try_recv() {
        if let EventMsg::RawResponseItem(event) = event.msg {
            raw_items.push(event.item);
        }
    }
    assert_eq!(raw_items, vec![expected.item]);

    let rollout_items = recorded
        .iter()
        .cloned()
        .map(RolloutItem::ResponseItem)
        .collect::<Vec<_>>();
    let reconstructed = session
        .reconstruct_history_from_rollout(&turn_context, &rollout_items)
        .await;
    assert_eq!(reconstructed.history, recorded);
}

/// Makes a taskless active turn, as a finished task leaves it, and injects one item into it.
async fn inject_into_finished_turn(
    session: &Session,
    turn_context: &TurnContext,
) -> (Arc<Mutex<TurnState>>, Vec<TurnInput>) {
    let item = ResponseItem::ConfigurationUpdate {
        reasoning: ConfigurationReasoning {
            effort: ReasoningEffort::High,
        },
    };
    let finished = ActiveTurn::default();
    let finished_state = Arc::clone(&finished.turn_state);
    *session.active_turn.lock().await = Some(finished);
    session
        .inject_client_response_items(vec![item.clone()], turn_context)
        .await;
    let expected = vec![TurnInput::ResponseItem(
        session.annotate_client_response_item(item),
    )];
    (finished_state, expected)
}

#[tokio::test]
async fn release_finished_turn_state_clears_turn_and_returns_late_input() {
    let (session, turn_context, _rx_event) = make_session_and_context_with_rx().await;
    let (finished_state, expected) = inject_into_finished_turn(&session, &turn_context).await;

    let released = session.release_finished_turn_state(&finished_state).await;

    assert_eq!(released, (true, expected));
    assert!(session.active_turn.lock().await.is_none());
}

#[tokio::test]
async fn release_finished_turn_state_returns_late_input_when_a_reservation_replaced_it() {
    let (session, turn_context, _rx_event) = make_session_and_context_with_rx().await;
    let (finished_state, expected) = inject_into_finished_turn(&session, &turn_context).await;
    let successor = ActiveTurn::default();
    let successor_state = Arc::clone(&successor.turn_state);
    *session.active_turn.lock().await = Some(successor);

    let released = session.release_finished_turn_state(&finished_state).await;

    assert_eq!(released, (false, expected.clone()));
    let successor_input = session
        .input_queue
        .take_pending_input_for_turn_state(successor_state.as_ref())
        .await;
    assert_eq!(successor_input, Vec::<TurnInput>::new());

    // A rejected start clears the reservation without draining it. The late input survives
    // because the release returned it to the caller.
    *session.active_turn.lock().await = None;
    assert_eq!(released.1, expected);
}

#[tokio::test]
async fn release_finished_turn_state_returns_late_input_when_turn_already_cleared() {
    let (session, turn_context, _rx_event) = make_session_and_context_with_rx().await;
    let (finished_state, expected) = inject_into_finished_turn(&session, &turn_context).await;
    *session.active_turn.lock().await = None;

    let released = session.release_finished_turn_state(&finished_state).await;

    assert_eq!(released, (false, expected));
}
