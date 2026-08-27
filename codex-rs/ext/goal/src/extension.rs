use std::sync::Arc;
use std::sync::Weak;

use codex_analytics::AnalyticsEventsClient;
use codex_core::ThreadManager;
use codex_extension_api::ConfigContributor;
use codex_extension_api::ExtensionData;
use codex_extension_api::ExtensionEventSink;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_extension_api::ThreadIdleCause;
use codex_extension_api::ThreadIdleInput;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadResumeInput;
use codex_extension_api::ThreadStartInput;
use codex_extension_api::ThreadStopInput;
use codex_extension_api::TokenUsageContributor;
use codex_extension_api::ToolCallOutcome;
use codex_extension_api::ToolContributor;
use codex_extension_api::ToolFinishInput;
use codex_extension_api::ToolLifecycleContributor;
use codex_extension_api::ToolLifecycleFuture;
use codex_extension_api::TurnAbortInput;
use codex_extension_api::TurnErrorInput;
use codex_extension_api::TurnLifecycleContributor;
use codex_extension_api::TurnStartInput;
use codex_extension_api::TurnStopInput;
use codex_extension_api::TurnSuspendInput;
use codex_otel::MetricsClient;
use codex_protocol::ThreadId;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::ThreadGoalStatus;
use codex_protocol::protocol::TokenUsageInfo;
use codex_protocol::turn_input::IdleTurnSource;
use codex_queue_extension::QueuedItemService;

use crate::accounting::BudgetLimitedGoalDisposition;
use crate::accounting::GoalAccountingState;
use crate::analytics::GoalAnalytics;
use crate::api::GoalService;
use crate::events::GoalEventEmitter;
use crate::metrics::GoalMetrics;
use crate::runtime::ActiveGoalStopReason;
use crate::runtime::GoalContinuationOutcome;
use crate::runtime::GoalRuntimeConfig;
use crate::runtime::GoalRuntimeHandle;
use crate::spec::UPDATE_GOAL_TOOL_NAME;
use crate::steering::budget_limit_steering_item;
use crate::tool::GoalToolExecutor;

#[derive(Clone, Debug)]
pub struct GoalExtensionConfig {
    pub enabled: bool,
    pub max_goal_token_budget: Option<i64>,
}
/// Host capability for starting a follow-up goal turn when a thread becomes idle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GoalAutoContinueCapability {
    /// Keep active goals inert while the thread is idle.
    #[default]
    Disabled,
    /// Allow the existing interactive idle-continuation path.
    Interactive,
}

fn auto_continue_capability_for(
    session_source: &SessionSource,
    host_capability: GoalAutoContinueCapability,
) -> GoalAutoContinueCapability {
    if session_source.is_non_root_agent() {
        GoalAutoContinueCapability::Disabled
    } else {
        host_capability
    }
}

fn continuation_outcome_warning(outcome: &GoalContinuationOutcome) -> Option<String> {
    match outcome {
        GoalContinuationOutcome::Rejected { reason } => {
            Some(format!("Goal continuation rejected: {reason}"))
        }
        GoalContinuationOutcome::Failed { error } => {
            Some(format!("Goal continuation failed: {error}"))
        }
        GoalContinuationOutcome::Started { .. }
        | GoalContinuationOutcome::DeferredForUserInput
        | GoalContinuationOutcome::DisabledForHost => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_continue_follows_current_host_for_root_threads() {
        for session_source in [
            SessionSource::Cli,
            SessionSource::VSCode,
            SessionSource::Exec,
            SessionSource::Mcp,
            SessionSource::Unknown,
        ] {
            assert_eq!(
                GoalAutoContinueCapability::Interactive,
                auto_continue_capability_for(
                    &session_source,
                    GoalAutoContinueCapability::Interactive,
                )
            );
        }
        assert_eq!(
            GoalAutoContinueCapability::Disabled,
            auto_continue_capability_for(
                &SessionSource::Exec,
                GoalAutoContinueCapability::Disabled,
            )
        );
        assert_eq!(
            GoalAutoContinueCapability::Disabled,
            auto_continue_capability_for(
                &SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                    parent_thread_id: ThreadId::new(),
                    depth: 1,
                    agent_path: None,
                    agent_nickname: None,
                    agent_role: None,
                }),
                GoalAutoContinueCapability::Interactive,
            )
        );
    }

    #[test]
    fn idle_continuation_respects_interrupts() {
        assert!(goal_continuation_is_allowed(&ThreadIdleInput {
            cause: ThreadIdleCause::Completed,
            session_store: &ExtensionData::new("session"),
            thread_store: &ExtensionData::new("thread"),
        }));
        assert!(!goal_continuation_is_allowed(&ThreadIdleInput {
            cause: ThreadIdleCause::Interrupted,
            session_store: &ExtensionData::new("session"),
            thread_store: &ExtensionData::new("thread"),
        }));
    }

    #[test]
    fn continuation_outcomes_surface_rejections_and_failures() {
        assert_eq!(
            continuation_outcome_warning(&GoalContinuationOutcome::Rejected {
                reason: "pending trigger".to_string(),
            }),
            Some("Goal continuation rejected: pending trigger".to_string())
        );
        assert_eq!(
            continuation_outcome_warning(&GoalContinuationOutcome::Failed {
                error: "storage unavailable".to_string(),
            }),
            Some("Goal continuation failed: storage unavailable".to_string())
        );
        assert_eq!(
            continuation_outcome_warning(&GoalContinuationOutcome::DeferredForUserInput),
            None
        );
    }
}

#[derive(Clone)]
pub struct GoalExtension<C> {
    state_dbs: Arc<codex_state::StateRuntime>,
    analytics: GoalAnalytics,
    event_emitter: GoalEventEmitter,
    metrics: GoalMetrics,
    thread_manager: Weak<ThreadManager>,
    goal_service: Arc<GoalService>,
    goal_config: Arc<dyn Fn(&C) -> GoalExtensionConfig + Send + Sync>,
    queue_service: Option<Arc<QueuedItemService>>,
    auto_continue_capability: GoalAutoContinueCapability,
}

impl<C> std::fmt::Debug for GoalExtension<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoalExtension").finish_non_exhaustive()
    }
}

impl<C> GoalExtension<C> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_host_capabilities(
        state_dbs: Arc<codex_state::StateRuntime>,
        analytics_events_client: AnalyticsEventsClient,
        event_sink: Arc<dyn ExtensionEventSink>,
        metrics_client: Option<MetricsClient>,
        thread_manager: Weak<ThreadManager>,
        goal_service: Arc<GoalService>,
        queue_service: Option<Arc<QueuedItemService>>,
        auto_continue_capability: GoalAutoContinueCapability,
        goal_config: impl Fn(&C) -> GoalExtensionConfig + Send + Sync + 'static,
    ) -> Self {
        Self {
            state_dbs,
            analytics: GoalAnalytics::new(analytics_events_client),
            event_emitter: GoalEventEmitter::new(event_sink),
            metrics: GoalMetrics::new(metrics_client),
            thread_manager,
            goal_service,
            queue_service,
            auto_continue_capability,
            goal_config: Arc::new(goal_config),
        }
    }
}

impl<C> ThreadLifecycleContributor<C> for GoalExtension<C>
where
    C: Send + Sync + 'static,
{
    fn on_thread_start<'a>(&'a self, input: ThreadStartInput<'a, C>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let config = (self.goal_config)(input.config);
            let enabled = config.enabled;
            let tools_available_for_thread = input.persistent_thread_state_available
                && !matches!(
                    input.session_source,
                    SessionSource::SubAgent(SubAgentSource::Review)
                );
            let host_capability = input
                .thread_store
                .get::<GoalAutoContinueCapability>()
                .map_or(self.auto_continue_capability, |capability| *capability);
            let auto_continue_capability =
                auto_continue_capability_for(input.session_source, host_capability);
            input.thread_store.insert(config);
            let accounting_state = input
                .thread_store
                .get_or_init::<GoalAccountingState>(GoalAccountingState::default);
            let Ok(thread_id) = ThreadId::from_string(input.thread_store.level_id()) else {
                return;
            };
            let runtime = input.thread_store.get_or_init::<GoalRuntimeHandle>(|| {
                GoalRuntimeHandle::new(
                    thread_id,
                    Arc::clone(&self.state_dbs),
                    self.event_emitter.clone(),
                    self.metrics.clone(),
                    self.thread_manager.clone(),
                    self.queue_service.clone(),
                    accounting_state,
                    GoalRuntimeConfig {
                        analytics: self.analytics.clone(),
                        enabled,
                        tools_available_for_thread,
                        auto_continue_capability,
                    },
                )
            });
            runtime.set_enabled(enabled);
            self.goal_service.register_runtime(&runtime);
        })
    }

    fn on_thread_resume<'a>(&'a self, input: ThreadResumeInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Some(runtime) = goal_runtime_handle(input.thread_store) else {
                return;
            };
            if let Some(capability) = input.thread_store.get::<GoalAutoContinueCapability>() {
                runtime.set_auto_continue_capability(*capability);
            }

            if let Err(err) = runtime.restore_after_resume().await {
                tracing::warn!(
                    "failed to restore goal runtime after thread resume for {}: {err}",
                    runtime.thread_id()
                );
            }
        })
    }

    fn on_thread_idle<'a>(&'a self, input: ThreadIdleInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if !goal_continuation_is_allowed(&input) {
                return;
            }
            let Some(runtime) = goal_runtime_handle(input.thread_store) else {
                return;
            };
            let outcome = match runtime.continue_if_idle().await {
                Ok(outcome) => outcome,
                Err(error) => GoalContinuationOutcome::Failed { error },
            };
            if let Some(message) = continuation_outcome_warning(&outcome) {
                runtime.emit_warning(message);
            }
        })
    }

    fn on_thread_stop<'a>(&'a self, input: ThreadStopInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            if let Some(runtime) = goal_runtime_handle(input.thread_store) {
                self.goal_service.unregister_runtime(&runtime);
            }
        })
    }
}

impl<C> ConfigContributor<C> for GoalExtension<C>
where
    C: Send + Sync + 'static,
{
    fn on_config_changed(
        &self,
        _session_store: &ExtensionData,
        thread_store: &ExtensionData,
        _previous_config: &C,
        new_config: &C,
    ) {
        let config = (self.goal_config)(new_config);
        let enabled = config.enabled;
        thread_store.insert(config);
        if let Some(runtime) = goal_runtime_handle(thread_store) {
            runtime.set_enabled(enabled);
        }
    }
}

impl<C> TurnLifecycleContributor for GoalExtension<C>
where
    C: Send + Sync + 'static,
{
    fn on_turn_start<'a>(&'a self, input: TurnStartInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Some(runtime) = goal_runtime_handle(input.thread_store) else {
                return;
            };
            if !runtime.is_enabled() {
                return;
            }

            if let Err(err) = self
                .state_dbs
                .thread_goals()
                .clear_thread_goal_continuation_deferral(runtime.thread_id())
                .await
            {
                tracing::warn!("failed to clear deferred goal continuation: {err}");
            }

            let idle_turn_source = input
                .turn_store
                .get::<IdleTurnSource>()
                .map_or(IdleTurnSource::Unspecified, |source| *source);
            let accounting = runtime.accounting_state();
            accounting.start_turn(
                input.turn_id,
                input.collaboration_mode.mode,
                idle_turn_source,
                input.token_usage_at_turn_start,
            );
            if matches!(
                input.collaboration_mode.mode,
                codex_protocol::config_types::ModeKind::Plan
            ) && idle_turn_source != IdleTurnSource::GoalContinuation
            {
                accounting.clear_current_turn_goal();
                return;
            }
            let Ok(goal) = self
                .state_dbs
                .thread_goals()
                .get_thread_goal(runtime.thread_id())
                .await
            else {
                return;
            };
            if let Some(goal) = goal
                && matches!(
                    goal.status,
                    codex_state::ThreadGoalStatus::Active
                        | codex_state::ThreadGoalStatus::BudgetLimited
                )
            {
                accounting.mark_turn_goal_active(input.turn_id, goal.goal_id);
            }
        })
    }

    fn on_turn_suspend<'a>(
        &'a self,
        input: TurnSuspendInput<'a>,
    ) -> ExtensionFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let Some(runtime) = goal_runtime_handle(input.thread_store) else {
                return Ok(());
            };
            if !runtime.is_enabled() {
                return Ok(());
            }

            let turn_id = input.turn_store.level_id();
            runtime
                .account_active_goal_progress(
                    turn_id,
                    &format!("{turn_id}:turn-suspend"),
                    codex_state::GoalAccountingMode::ActiveOnly,
                    BudgetLimitedGoalDisposition::KeepActive,
                )
                .await
                .map(|_| ())
        })
    }

    fn on_turn_stop<'a>(&'a self, input: TurnStopInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Some(runtime) = goal_runtime_handle(input.thread_store) else {
                return;
            };
            if !runtime.is_enabled() {
                return;
            }

            let turn_id = input.turn_store.level_id();
            if let Err(err) = runtime
                .account_active_goal_progress(
                    turn_id,
                    &format!("{turn_id}:turn-stop"),
                    codex_state::GoalAccountingMode::ActiveOnly,
                    BudgetLimitedGoalDisposition::ClearActive,
                )
                .await
            {
                tracing::warn!(
                    "failed to account active goal progress at turn stop for {turn_id}: {err}"
                );
                return;
            }
            runtime.accounting_state().finish_turn(turn_id);
        })
    }

    fn on_turn_abort<'a>(&'a self, input: TurnAbortInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Some(runtime) = goal_runtime_handle(input.thread_store) else {
                return;
            };
            if !runtime.is_enabled() {
                return;
            }

            let turn_id = input.turn_store.level_id();
            if let Err(err) = runtime
                .account_active_goal_progress(
                    turn_id,
                    &format!("{turn_id}:turn-abort"),
                    codex_state::GoalAccountingMode::ActiveOnly,
                    BudgetLimitedGoalDisposition::ClearActive,
                )
                .await
            {
                tracing::warn!(
                    "failed to account active goal progress after turn abort for {turn_id}: {err}"
                );
                return;
            }
            runtime.accounting_state().finish_turn(turn_id);
        })
    }

    fn on_turn_error<'a>(&'a self, input: TurnErrorInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Some(runtime) = goal_runtime_handle(input.thread_store) else {
                return;
            };

            let reason = match input.error {
                CodexErrorInfo::UsageLimitExceeded => ActiveGoalStopReason::UsageLimit,
                // The turn has ended because the error was non-retryable or its
                // retries were exhausted. Block the goal to prevent automatic
                // continuation from looping and consuming tokens, as can happen
                // with compaction errors.
                _ => ActiveGoalStopReason::TurnError,
            };
            if let Err(err) = runtime
                .stop_active_goal_for_turn(input.turn_id, reason)
                .await
            {
                tracing::warn!(
                    error = ?input.error,
                    "failed to stop active goal after turn error: {err}"
                );
            }
        })
    }
}

impl<C> TokenUsageContributor for GoalExtension<C>
where
    C: Send + Sync + 'static,
{
    fn on_token_usage<'a>(
        &'a self,
        _session_store: &'a ExtensionData,
        thread_store: &'a ExtensionData,
        turn_store: &'a ExtensionData,
        token_usage: &'a TokenUsageInfo,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Some(runtime) = goal_runtime_handle(thread_store) else {
                return;
            };
            if !runtime.is_enabled() {
                return;
            }

            let Some(_recorded) = runtime
                .accounting_state()
                .record_token_usage(turn_store.level_id(), &token_usage.total_token_usage)
            else {
                return;
            };
        })
    }
}

impl<C> ToolLifecycleContributor for GoalExtension<C>
where
    C: Send + Sync + 'static,
{
    fn on_tool_finish<'a>(&'a self, input: ToolFinishInput<'a>) -> ToolLifecycleFuture<'a> {
        Box::pin(async move {
            let Some(runtime) = goal_runtime_handle(input.thread_store) else {
                return;
            };
            let should_count_for_goal_progress = runtime.is_enabled()
                && tool_attempt_counts_for_goal_progress(input.outcome)
                && !(input.tool_name.is_default_namespace()
                    && input.tool_name.name == UPDATE_GOAL_TOOL_NAME);
            if !should_count_for_goal_progress {
                return;
            }
            let turn_id = input.turn_id;
            let progress = match runtime
                .account_active_goal_progress(
                    turn_id,
                    input.call_id,
                    codex_state::GoalAccountingMode::ActiveOnly,
                    BudgetLimitedGoalDisposition::KeepActive,
                )
                .await
            {
                Ok(Some(progress)) => progress,
                Ok(None) => return,
                Err(err) => {
                    tracing::warn!(
                        "failed to account active goal progress after tool finish for {turn_id}: {err}"
                    );
                    return;
                }
            };
            let goal = progress.goal;
            if goal.status != ThreadGoalStatus::BudgetLimited {
                return;
            }
            if !runtime
                .accounting_state()
                .mark_budget_limit_reported_if_new(progress.goal_id.as_str())
            {
                return;
            }
            let item = budget_limit_steering_item(&goal);
            runtime.inject_active_turn_steering(item).await;
        })
    }
}

impl<C> ToolContributor for GoalExtension<C>
where
    C: Send + Sync + 'static,
{
    fn tools(
        &self,
        _session_store: &ExtensionData,
        thread_store: &ExtensionData,
    ) -> Vec<Arc<dyn codex_extension_api::ToolExecutor<codex_extension_api::ToolCall>>> {
        let Some(runtime) = goal_runtime_handle(thread_store) else {
            return Vec::new();
        };
        if !runtime.tools_visible() {
            return Vec::new();
        }
        let max_goal_token_budget = thread_store
            .get::<GoalExtensionConfig>()
            .and_then(|config| config.max_goal_token_budget);

        vec![
            Arc::new(GoalToolExecutor::get(
                runtime.thread_id(),
                Arc::clone(&self.state_dbs),
                runtime.accounting_state(),
                self.analytics.clone(),
                self.event_emitter.clone(),
                self.metrics.clone(),
            )),
            Arc::new(GoalToolExecutor::create(
                runtime.thread_id(),
                Arc::clone(&self.state_dbs),
                runtime.accounting_state(),
                self.analytics.clone(),
                self.event_emitter.clone(),
                self.metrics.clone(),
                max_goal_token_budget,
            )),
            Arc::new(GoalToolExecutor::update(
                runtime.thread_id(),
                Arc::clone(&self.state_dbs),
                runtime.accounting_state(),
                self.analytics.clone(),
                self.event_emitter.clone(),
                self.metrics.clone(),
            )),
        ]
    }
}

#[allow(clippy::too_many_arguments)]
pub fn install_with_backend<C>(
    registry: &mut ExtensionRegistryBuilder<C>,
    state_dbs: Arc<codex_state::StateRuntime>,
    analytics_events_client: AnalyticsEventsClient,
    metrics_client: Option<MetricsClient>,
    thread_manager: Weak<ThreadManager>,
    goal_service: Arc<GoalService>,
    queue_service: Option<Arc<QueuedItemService>>,
    auto_continue_capability: GoalAutoContinueCapability,
    goal_config: impl Fn(&C) -> GoalExtensionConfig + Send + Sync + 'static,
) where
    C: Send + Sync + 'static,
{
    let extension = Arc::new(GoalExtension::new_with_host_capabilities(
        state_dbs,
        analytics_events_client,
        registry.event_sink(),
        metrics_client,
        thread_manager,
        Arc::clone(&goal_service),
        queue_service,
        auto_continue_capability,
        goal_config,
    ));
    registry.thread_lifecycle_contributor(extension.clone());
    registry.config_contributor(extension.clone());
    registry.turn_lifecycle_contributor(extension.clone());
    registry.token_usage_contributor(extension.clone());
    registry.tool_lifecycle_contributor(extension.clone());
    registry.tool_contributor(extension);
}

fn goal_runtime_handle(thread_store: &ExtensionData) -> Option<Arc<GoalRuntimeHandle>> {
    thread_store.get::<GoalRuntimeHandle>()
}

fn goal_continuation_is_allowed(input: &ThreadIdleInput<'_>) -> bool {
    input.cause != ThreadIdleCause::Interrupted
}

fn tool_attempt_counts_for_goal_progress(outcome: ToolCallOutcome) -> bool {
    match outcome {
        ToolCallOutcome::Completed { .. } => true,
        ToolCallOutcome::Failed {
            handler_executed: true,
        } => true,
        ToolCallOutcome::Blocked
        | ToolCallOutcome::Failed {
            handler_executed: false,
        }
        | ToolCallOutcome::Aborted => false,
    }
}
