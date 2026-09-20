use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn::TurnStartCustody;
use crate::session::turn::run_turn;
use crate::session::turn_context::TurnContext;
use crate::session_startup_prewarm::SessionStartupPrewarmResolution;
use crate::state::TaskKind;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TurnStartedEvent;
use tokio::sync::Mutex;
use tracing::Instrument;
use tracing::trace_span;

use super::SessionTask;
use super::SessionTaskResult;

#[derive(Default)]
pub(crate) struct RegularTask {
    preparation: Mutex<Option<TurnStartCustody>>,
}

impl RegularTask {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl SessionTask for RegularTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.turn"
    }

    async fn abort(&self, _session: Arc<Session>, ctx: Arc<TurnContext>) {
        let custody = self.preparation.lock().await;
        if custody
            .as_ref()
            .is_some_and(TurnStartCustody::has_unresolved_input)
        {
            // No hook replay or accepted-input fallback. The original task is
            // still the owner here; post-retirement resolution remains a gap.
            tracing::warn!(turn_id = %ctx.sub_id, "original preparation input remains unresolved at task abort");
        }
    }

    async fn run(
        self: Arc<Self>,
        sess: Arc<Session>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        // RunningTask retains self through its original abort callback. Dropping
        // the run future releases this borrow, not its original input/state.
        let mut preparation = self.preparation.lock().await;
        *preparation = Some(TurnStartCustody::new(input));
        let run_turn_span = trace_span!("run_turn");
        // Regular turns emit `TurnStarted` inline so first-turn lifecycle does
        // not wait on startup prewarm resolution.
        let prewarmed_client_session = async {
            let event = EventMsg::TurnStarted(TurnStartedEvent {
                turn_id: ctx.sub_id.clone(),
                trace_id: ctx.trace_id.clone(),
                started_at: ctx.turn_timing_state.started_at_unix_secs().await,
                model_context_window: ctx.model_context_window(),
                collaboration_mode_kind: ctx.mode,
            });
            sess.send_event(ctx.as_ref(), event).await;
            sess.set_server_reasoning_included(/*included*/ false).await;
            sess.consume_startup_prewarm_for_regular_turn(&cancellation_token)
                .await
        }
        .instrument(trace_span!("regular_task.prepare_run_turn"))
        .await;
        let prewarmed_client_session = match prewarmed_client_session {
            SessionStartupPrewarmResolution::Cancelled => {
                preparation
                    .as_mut()
                    .expect("original task custody")
                    .record_initial_input(&sess, &ctx)
                    .await?;
                return Ok(None);
            }
            SessionStartupPrewarmResolution::Unavailable { .. } => None,
            SessionStartupPrewarmResolution::Ready(prewarmed_client_session) => {
                Some(*prewarmed_client_session)
            }
        };
        let mut prewarmed_client_session = prewarmed_client_session;
        loop {
            let last_agent_message = run_turn(
                Arc::clone(&sess),
                Arc::clone(&ctx),
                preparation.as_mut().expect("original task custody"),
                prewarmed_client_session.take(),
                cancellation_token.child_token(),
            )
            .instrument(run_turn_span.clone())
            .await?;
            if !sess.input_queue.has_pending_input(&sess.active_turn).await {
                return Ok(last_agent_message);
            }
            // Preserve the original unbound follow-up behavior after hook/skills
            // refusal. Only the original observation binding adds this fence.
            if sess
                .services
                .thread_extension_data
                .get::<crate::ObservationBinding>()
                .is_some()
                && preparation
                    .as_ref()
                    .expect("original task custody")
                    .has_unresolved_input()
            {
                return Err(crate::error::CodexErr::InvalidRequest(
                    "original turn input is unresolved; follow-up cannot replace its custody"
                        .to_owned(),
                ));
            }
            *preparation = Some(TurnStartCustody::new(Vec::new()));
        }
    }
}

#[cfg(test)]
#[path = "regular_tests.rs"]
mod tests;
