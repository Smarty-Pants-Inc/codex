//! Own the original first-step effects and their retained outputs. No preview.
use super::*;
use crate::context::world_state::WorldState;

/// This is materialization custody, not complete accounting or send permission.
pub(super) struct PreparedTurnStart {
    pub(super) first_step_context: Arc<StepContext>,
    pub(super) world_state: Arc<WorldState>,
    pub(super) display_roots: Vec<(String, PathUri)>,
    pub(super) can_drain_pending_input: bool,
    // Preserve the old operand before the original settings update below.
    pub(super) previous_turn_settings: Option<PreviousTurnSettings>,
}

/// Retained by the original RegularTask, not owned by its cancellable run future.
/// No restart/readback authority follows when the task itself is retired.
pub(crate) struct TurnStartCustody {
    pub(super) input: turn_start_input::TurnStartInput,
    pub(super) prepared: Option<PreparedTurnStart>,
    preparation_started: bool,
}

impl TurnStartCustody {
    pub(crate) fn new(input: Vec<TurnInput>) -> Self {
        Self {
            input: turn_start_input::TurnStartInput::new(input),
            prepared: None,
            preparation_started: false,
        }
    }

    pub(crate) async fn record_initial_input(
        &mut self,
        sess: &Arc<Session>,
        turn: &Arc<TurnContext>,
    ) -> CodexResult<bool> {
        self.input
            .record(sess, turn, PersistContext::Standard)
            .await
    }

    pub(crate) fn has_unresolved_input(&self) -> bool {
        !self.input.is_recorded()
    }

    pub(super) async fn prepare_once(
        &mut self,
        sess: &Arc<Session>,
        turn: &Arc<TurnContext>,
        cancellation: &CancellationToken,
    ) -> CodexResult<bool> {
        if self.prepared.is_some() {
            return Ok(true);
        }
        if self.preparation_started {
            return Err(CodexErr::InvalidRequest(
                "original turn preparation is incomplete; effects cannot be replayed".to_owned(),
            ));
        }
        self.preparation_started = true;
        self.prepared = prepare_turn_start(sess, turn, &mut self.input, cancellation).await?;
        Ok(self.prepared.is_some())
    }
}

#[instrument(level = "trace", skip_all)]
pub(super) async fn prepare_turn_start(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    input: &mut turn_start_input::TurnStartInput,
    cancellation_token: &CancellationToken,
) -> CodexResult<Option<PreparedTurnStart>> {
    let previous_turn_settings = sess.previous_turn_settings().await;
    let user_input = turn_user_input(input.original());
    let (required_servers, mentioned_plugins) =
        match required_mcp_servers_for_input(sess, turn_context.as_ref(), &user_input)
            .or_cancel(cancellation_token)
            .await
        {
            Ok(requirements) => requirements,
            Err(err) => {
                input
                    .record(sess, turn_context, PersistContext::Standard)
                    .await?;
                return Err(err.into());
            }
        };

    // run_turn owns the step used to seed context and make the first sampling request.
    let first_step_context = match sess
        .capture_step_context_with_required_mcp_servers(
            Arc::clone(turn_context),
            cancellation_token,
            &required_servers,
        )
        .await
    {
        Ok(step_context) => step_context,
        Err(err) if matches!(err.details(), CodexErrorDetails::TurnAborted) => {
            input
                .record(sess, turn_context, PersistContext::Standard)
                .await?;
            return Err(err);
        }
        Err(err) => return Err(err),
    };
    // Keep the exact model-visible state used by this turn and its inline compactions.
    let (world_state, display_roots) = tokio::join!(
        sess.record_context_updates_and_set_reference_context_item(first_step_context.as_ref()),
        async {
            if first_step_context
                .turn
                .config
                .features
                .enabled(Feature::CwdRelativeTurnDiffs)
            {
                first_step_context
                    .environments
                    .turn_environments()
                    .map(|environment| {
                        (
                            environment.selection().environment_id,
                            environment.cwd().clone(),
                        )
                    })
                    .collect()
            } else {
                turn_diff_display_roots(first_step_context.as_ref()).await
            }
        },
    );
    let world_state = world_state?;

    let Some((injection_items, explicitly_enabled_connectors)) = build_skills_and_plugins(
        sess,
        first_step_context.as_ref(),
        &user_input,
        &mentioned_plugins,
        cancellation_token,
    )
    .await
    else {
        return Ok(None);
    };

    if run_pending_session_start_hooks(sess, turn_context).await {
        return Ok(None);
    }
    let can_drain_pending_input = input.original().is_empty();
    if input
        .record(sess, turn_context, PersistContext::TurnStart)
        .await?
    {
        return Ok(None);
    }

    sess.merge_connector_selection(explicitly_enabled_connectors.clone())
        .await;
    sess.set_previous_turn_settings(Some(PreviousTurnSettings {
        model: turn_context.model_info.slug.clone(),
        comp_hash: turn_context.model_info.comp_hash.clone(),
        realtime_active: Some(turn_context.realtime_active),
    }))
    .await;
    for response_item in injection_items {
        sess.record_conversation_items(turn_context, std::slice::from_ref(&response_item))
            .await;
    }

    track_turn_resolved_config_analytics(sess, turn_context, input.original()).await;

    Ok(Some(PreparedTurnStart {
        first_step_context,
        world_state,
        display_roots,
        can_drain_pending_input,
        previous_turn_settings,
    }))
}

#[cfg(test)]
#[path = "turn_preparation_custody_tests.rs"]
mod custody_tests;
