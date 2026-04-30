//! Top-level TUI application state and runtime wiring.
//!
//! This module owns the `App` struct, shared imports, and the high-level run loop that coordinates
//! the focused app submodules.

use crate::app_backtrack::BacktrackState;
use crate::app_command::AppCommand;
use crate::app_command::AppCommandView;
use crate::app_event::AppEvent;
use crate::app_event::ExitMode;
use crate::app_event::FeedbackCategory;
use crate::app_event::RateLimitRefreshOrigin;
use crate::app_event::RealtimeAudioDeviceKind;
#[cfg(target_os = "windows")]
use crate::app_event::WindowsSandboxEnableMode;
use crate::app_event_sender::AppEventSender;
use crate::app_server_approval_conversions::network_approval_context_to_core;
use crate::app_server_session::AppServerSession;
use crate::app_server_session::AppServerStartedThread;
use crate::app_server_session::ThreadSessionState;
use crate::app_server_session::app_server_rate_limit_snapshots_to_core;
use crate::bottom_pane::ApprovalRequest;
use crate::bottom_pane::ColumnWidthMode;
use crate::bottom_pane::FeedbackAudience;
use crate::bottom_pane::McpServerElicitationFormRequest;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionShortcut;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::chatwidget::ChatWidget;
use crate::chatwidget::ExternalEditorState;
use crate::chatwidget::ReplayKind;
use crate::chatwidget::ThreadInputState;
use crate::cwd_prompt::CwdPromptAction;
use crate::diff_render::DiffSummary;
use crate::exec_command::split_command_string;
use crate::exec_command::strip_bash_lc_and_escape;
use crate::external_agent_config_migration_startup::ExternalAgentConfigMigrationStartupOutcome;
use crate::external_agent_config_migration_startup::handle_external_agent_config_migration_prompt_if_needed;
use crate::external_editor;
use crate::file_search::FileSearchManager;
use crate::history_cell;
use crate::history_cell::HistoryCell;
#[cfg(not(debug_assertions))]
use crate::history_cell::UpdateAvailableHistoryCell;
use crate::key_hint;
use crate::legacy_core::append_message_history_entry;
use crate::legacy_core::config::Config;
use crate::legacy_core::config::ConfigBuilder;
use crate::legacy_core::config::ConfigOverrides;
use crate::legacy_core::config::edit::ConfigEdit;
use crate::legacy_core::config::edit::ConfigEditsBuilder;
use crate::legacy_core::lookup_message_history_entry;
use crate::legacy_core::plugins::PluginsManager;
#[cfg(target_os = "windows")]
use crate::legacy_core::windows_sandbox::WindowsSandboxLevelExt;
use crate::model_catalog::ModelCatalog;
use crate::model_migration::ModelMigrationOutcome;
use crate::model_migration::migration_copy_for_models;
use crate::model_migration::run_model_migration_prompt;
use crate::multi_agents::agent_picker_status_dot_spans;
use crate::multi_agents::format_agent_picker_item_name;
use crate::multi_agents::next_agent_shortcut_matches;
use crate::multi_agents::previous_agent_shortcut_matches;
use crate::multi_agents::system_event_cell;
use crate::oracle_attachments::resolve_oracle_attachments;
use crate::oracle_broker::OracleBrokerClient;
use crate::oracle_broker::OracleBrokerRequest;
use crate::oracle_broker::OracleBrokerThreadEntry;
use crate::oracle_broker::OracleBrokerThreadHistoryEntry;
use crate::oracle_broker::OracleBrokerThreadHistoryResponse;
use crate::oracle_broker::OracleBrokerThreadHistoryWindow;
use crate::oracle_broker::OracleBrokerThreadOpenResponse;
use crate::oracle_broker::spawn_oracle_broker;
use crate::oracle_supervisor::ORACLE_CONTROL_SCHEMA_VERSION;
use crate::oracle_supervisor::OracleAction;
use crate::oracle_supervisor::OracleCommand;
use crate::oracle_supervisor::OracleControlDirective;
#[cfg(test)]
use crate::oracle_supervisor::OracleControlOp;
use crate::oracle_supervisor::OracleModelPreset;
use crate::oracle_supervisor::OracleParticipantVisibility;
use crate::oracle_supervisor::OracleRequestKind;
use crate::oracle_supervisor::OracleRoutedThreadOwner;
#[cfg(test)]
use crate::oracle_supervisor::OracleRoutingParticipant;
use crate::oracle_supervisor::OracleRunRequest;
use crate::oracle_supervisor::OracleRunResult;
use crate::oracle_supervisor::OracleSessionOwnership;
use crate::oracle_supervisor::OracleSupervisorPhase;
use crate::oracle_supervisor::OracleSupervisorState;
use crate::oracle_supervisor::OracleThreadBinding;
use crate::oracle_supervisor::OracleWorkflowBinding;
use crate::oracle_supervisor::OracleWorkflowMode;
use crate::oracle_supervisor::OracleWorkflowStatus;
use crate::oracle_supervisor::build_checkpoint_prompt;
use crate::oracle_supervisor::build_context_prompt;
use crate::oracle_supervisor::build_control_repair_prompt;
use crate::oracle_supervisor::build_oracle_workflow_reminder;
use crate::oracle_supervisor::build_user_turn_prompt;
use crate::oracle_supervisor::find_oracle_repo;
use crate::oracle_supervisor::generate_session_slug;
use crate::oracle_supervisor::oracle_delegate_task_from_directive;
use crate::oracle_supervisor::oracle_history_cell;
use crate::oracle_supervisor::orchestrator_developer_instructions;
use crate::oracle_supervisor::parse_oracle_response;
use crate::oracle_supervisor::resolve_context_requests;
use crate::oracle_supervisor::summarize_thread;
use crate::oracle_supervisor::user_turn_requires_orchestrator_control;
use crate::pager_overlay::Overlay;
use crate::read_session_model;
use crate::render::highlight::highlight_bash_to_lines;
use crate::render::renderable::Renderable;
use crate::resume_picker::SessionSelection;
use crate::resume_picker::SessionTarget;
#[cfg(test)]
use crate::test_support::PathBufExt;
#[cfg(test)]
use crate::test_support::test_path_buf;
#[cfg(test)]
use crate::test_support::test_path_display;
use crate::transcript_reflow::TranscriptReflowState;
use crate::tui;
use crate::tui::TuiEvent;
use crate::update_action::UpdateAction;
use crate::version::CODEX_CLI_VERSION;
use codex_ansi_escape::ansi_escape_line;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::AddCreditsNudgeCreditType;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::CodexErrorInfo as AppServerCodexErrorInfo;
use codex_app_server_protocol::ConfigLayerSource;
use codex_app_server_protocol::ConfigValueWriteParams;
use codex_app_server_protocol::ConfigWriteResponse;
use codex_app_server_protocol::FeedbackUploadParams;
use codex_app_server_protocol::FeedbackUploadResponse;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::ListMcpServerStatusParams;
use codex_app_server_protocol::ListMcpServerStatusResponse;
use codex_app_server_protocol::McpServerStatus;
use codex_app_server_protocol::McpServerStatusDetail;
use codex_app_server_protocol::MergeStrategy;
use codex_app_server_protocol::PluginInstallParams;
use codex_app_server_protocol::PluginInstallResponse;
use codex_app_server_protocol::PluginListParams;
use codex_app_server_protocol::PluginListResponse;
use codex_app_server_protocol::PluginReadParams;
use codex_app_server_protocol::PluginReadResponse;
use codex_app_server_protocol::PluginUninstallParams;
use codex_app_server_protocol::PluginUninstallResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SendAddCreditsNudgeEmailParams;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::SkillsListParams;
use codex_app_server_protocol::SkillsListResponse;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadLoadedListParams;
use codex_app_server_protocol::ThreadMemoryMode;
use codex_app_server_protocol::ThreadRollbackResponse;
use codex_app_server_protocol::ThreadStartSource;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnError as AppServerTurnError;
use codex_app_server_protocol::TurnStatus;
use codex_config::ConfigLayerStackOrdering;
use codex_config::types::ApprovalsReviewer;
use codex_config::types::ModelAvailabilityNuxConfig;
use codex_exec_server::EnvironmentManager;
use codex_features::Feature;
use codex_models_manager::collaboration_mode_presets::CollaborationModesConfig;
use codex_models_manager::model_presets::HIDE_GPT_5_1_CODEX_MAX_MIGRATION_PROMPT_CONFIG;
use codex_models_manager::model_presets::HIDE_GPT5_1_MIGRATION_PROMPT_CONFIG;
use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::approvals::ExecApprovalRequestEvent;
use codex_protocol::config_types::Personality;
#[cfg(target_os = "windows")]
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::openai_models::ModelAvailabilityNux;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelUpgrade;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::FinalOutput;
use codex_protocol::protocol::GetHistoryEntryResponseEvent;
use codex_protocol::protocol::ListSkillsResponseEvent;
#[cfg(test)]
use codex_protocol::protocol::McpAuthStatus;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SkillErrorInfo;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::user_input::UserInput;
use codex_terminal_detection::user_agent;
use codex_utils_absolute_path::AbsolutePathBuf;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::backend::Backend;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tokio::process::Command;
use tokio::select;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::unbounded_channel;
use tokio::task::JoinHandle;
use toml::Value as TomlValue;
use url::Url;
use uuid::Uuid;
mod agent_navigation;
mod app_server_adapter;
pub(crate) mod app_server_requests;
mod background_requests;
mod config_persistence;
mod event_dispatch;
mod history_ui;
mod input;
mod loaded_threads;
mod pending_interactive_replay;
mod platform_actions;
mod replay_filter;
mod resize_reflow;
mod session_lifecycle;
mod side;
mod startup_prompts;
mod thread_events;
mod thread_goal_actions;
mod thread_routing;
mod thread_session_state;

use self::agent_navigation::AgentNavigationDirection;
use self::agent_navigation::AgentNavigationState;
use self::app_server_requests::PendingAppServerRequests;
use self::loaded_threads::find_loaded_subagent_threads_for_primary;
use self::pending_interactive_replay::PendingInteractiveReplayState;
use self::platform_actions::*;
use self::side::SideParentStatus;
use self::side::SideParentStatusChange;
use self::side::SideThreadState;
use self::startup_prompts::*;
use self::thread_events::*;

const EXTERNAL_EDITOR_HINT: &str = "Save and close external editor to continue.";
const THREAD_EVENT_CHANNEL_CAPACITY: usize = 32768;
type OracleRunRemoteMetadata = (Option<String>, Option<String>);
type OracleRunRemoteMetadataMap = HashMap<String, OracleRunRemoteMetadata>;

static ORACLE_RUN_REMOTE_METADATA: LazyLock<StdMutex<OracleRunRemoteMetadataMap>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

fn remember_oracle_run_remote_metadata(
    run_id: &str,
    conversation_id: Option<String>,
    remote_title: Option<String>,
) {
    let remote_title = remote_title.filter(|title| !title.trim().is_empty());
    if conversation_id.is_none() && remote_title.is_none() {
        return;
    }
    let mut metadata = ORACLE_RUN_REMOTE_METADATA
        .lock()
        .expect("oracle run remote metadata lock poisoned");
    metadata.insert(run_id.to_string(), (conversation_id, remote_title));
}

fn take_oracle_run_remote_metadata(run_id: &str) -> (Option<String>, Option<String>) {
    ORACLE_RUN_REMOTE_METADATA
        .lock()
        .expect("oracle run remote metadata lock poisoned")
        .remove(run_id)
        .unwrap_or_default()
}

enum ThreadInteractiveRequest {
    Approval(ApprovalRequest),
    McpServerElicitation(McpServerElicitationFormRequest),
}

fn app_server_request_id_to_mcp_request_id(
    request_id: &codex_app_server_protocol::RequestId,
) -> codex_protocol::mcp::RequestId {
    match request_id {
        codex_app_server_protocol::RequestId::String(value) => {
            codex_protocol::mcp::RequestId::String(value.clone())
        }
        codex_app_server_protocol::RequestId::Integer(value) => {
            codex_protocol::mcp::RequestId::Integer(*value)
        }
    }
}

fn command_execution_decision_to_review_decision(
    decision: codex_app_server_protocol::CommandExecutionApprovalDecision,
) -> codex_protocol::protocol::ReviewDecision {
    match decision {
        codex_app_server_protocol::CommandExecutionApprovalDecision::Accept => {
            codex_protocol::protocol::ReviewDecision::Approved
        }
        codex_app_server_protocol::CommandExecutionApprovalDecision::AcceptForSession => {
            codex_protocol::protocol::ReviewDecision::ApprovedForSession
        }
        codex_app_server_protocol::CommandExecutionApprovalDecision::AcceptWithExecpolicyAmendment {
            execpolicy_amendment,
        } => codex_protocol::protocol::ReviewDecision::ApprovedExecpolicyAmendment {
            proposed_execpolicy_amendment: execpolicy_amendment.into_core(),
        },
        codex_app_server_protocol::CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
            network_policy_amendment,
        } => codex_protocol::protocol::ReviewDecision::NetworkPolicyAmendment {
            network_policy_amendment: network_policy_amendment.into_core(),
        },
        codex_app_server_protocol::CommandExecutionApprovalDecision::Decline => {
            codex_protocol::protocol::ReviewDecision::Denied
        }
        codex_app_server_protocol::CommandExecutionApprovalDecision::Cancel => {
            codex_protocol::protocol::ReviewDecision::Abort
        }
    }
}

/// Extracts `receiver_thread_ids` from collab agent tool-call notifications.
///
/// Only `ItemStarted` and `ItemCompleted` notifications with a `CollabAgentToolCall` item carry
/// receiver thread ids. All other notification variants return `None`.
fn collab_receiver_thread_ids(notification: &ServerNotification) -> Option<&[String]> {
    match notification {
        ServerNotification::ItemStarted(notification) => match &notification.item {
            ThreadItem::CollabAgentToolCall {
                receiver_thread_ids,
                ..
            } => Some(receiver_thread_ids),
            _ => None,
        },
        ServerNotification::ItemCompleted(notification) => match &notification.item {
            ThreadItem::CollabAgentToolCall {
                receiver_thread_ids,
                ..
            } => Some(receiver_thread_ids),
            _ => None,
        },
        _ => None,
    }
}

fn default_exec_approval_decisions(
    network_approval_context: Option<&codex_protocol::protocol::NetworkApprovalContext>,
    proposed_execpolicy_amendment: Option<&codex_protocol::approvals::ExecPolicyAmendment>,
    proposed_network_policy_amendments: Option<
        &[codex_protocol::approvals::NetworkPolicyAmendment],
    >,
    additional_permissions: Option<&codex_protocol::models::AdditionalPermissionProfile>,
) -> Vec<codex_protocol::protocol::ReviewDecision> {
    ExecApprovalRequestEvent::default_available_decisions(
        network_approval_context,
        proposed_execpolicy_amendment,
        proposed_network_policy_amendments,
        additional_permissions,
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AutoReviewMode {
    approval_policy: AskForApproval,
    approvals_reviewer: ApprovalsReviewer,
    sandbox_policy: SandboxPolicy,
}

/// Enabling the Auto-review experiment in the TUI should also switch the
/// current `/approvals` settings to the matching Auto-review mode. Users
/// can still change `/approvals` afterward; this just assumes that opting into
/// the experiment means they want Auto-review enabled immediately.
fn auto_review_mode() -> AutoReviewMode {
    AutoReviewMode {
        approval_policy: AskForApproval::OnRequest,
        approvals_reviewer: ApprovalsReviewer::AutoReview,
        sandbox_policy: SandboxPolicy::new_workspace_write_policy(),
    }
}
/// Baseline cadence for periodic stream commit animation ticks.
///
/// Smooth-mode streaming drains one line per tick, so this interval controls
/// perceived typing speed for non-backlogged output.
const COMMIT_ANIMATION_TICK: Duration = tui::TARGET_FRAME_INTERVAL;
const EXIT_AFTER_TURN_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(test)]
fn try_enqueue_commit_tick(tx: &AppEventSender, pending: &AtomicBool) -> bool {
    if pending
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }

    let event = AppEvent::CommitTick;
    crate::session_log::log_inbound_app_event(&event);
    if let Err(err) = tx.app_event_tx.send(event) {
        pending.store(false, Ordering::Release);
        tracing::error!("failed to send event: {err}");
        return false;
    }

    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OracleNewThreadBinding {
    thread_id: ThreadId,
    title: String,
    attached_remote: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OracleAttachThreadBinding {
    ReattachedLocal {
        thread_id: ThreadId,
        title: String,
        conversation_id: String,
    },
    AttachedRemote {
        thread_id: ThreadId,
        title: String,
        conversation_id: String,
        history: Vec<OracleBrokerThreadHistoryEntry>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OracleHistoryImportOutcome {
    Imported { message_count: usize },
    NoNewMessages,
    SkippedToAvoidConflict,
}

const ORACLE_HUMAN_DESTINATION: &str = "human";
const ORACLE_ORCHESTRATOR_DESTINATION: &str = "orchestrator";
const AGENT_SELECTION_VIEW_ID: &str = "agent-selection";
const ORACLE_SELECTION_VIEW_ID: &str = "oracle-selection";
const ORACLE_WORKFLOW_SELECTION_VIEW_ID: &str = "oracle-workflow-selection";
const CODEX_TUI_STARTUP_ACTION_ENV: &str = "CODEX_TUI_STARTUP_ACTION";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupUiAction {
    OpenOraclePicker,
}

impl StartupUiAction {
    fn from_env() -> Option<Self> {
        let value = std::env::var(CODEX_TUI_STARTUP_ACTION_ENV).ok()?;
        match value.trim().to_ascii_lowercase().as_str() {
            "oracle-picker" | "oracle_picker" => Some(Self::OpenOraclePicker),
            _ => None,
        }
    }
}

impl OracleAttachThreadBinding {
    fn thread_id(&self) -> ThreadId {
        match self {
            Self::ReattachedLocal { thread_id, .. } | Self::AttachedRemote { thread_id, .. } => {
                *thread_id
            }
        }
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
struct OracleTestBrokerHooks {
    list_thread_results: VecDeque<Result<Vec<OracleBrokerThreadEntry>, String>>,
    list_thread_delay: Option<Duration>,
    new_thread_results: VecDeque<Result<OracleBrokerThreadOpenResponse, String>>,
    new_thread_delay: Option<Duration>,
    attach_thread_results: VecDeque<Result<OracleBrokerThreadOpenResponse, String>>,
    attach_thread_delay: Option<Duration>,
    thread_history_results: VecDeque<Result<OracleBrokerThreadHistoryResponse, String>>,
    list_thread_calls: usize,
    list_thread_followup_sessions: Vec<Option<String>>,
    new_thread_calls: usize,
    new_thread_followup_sessions: Vec<Option<String>>,
    attach_thread_calls: Vec<String>,
    attach_thread_followup_sessions: Vec<Option<String>>,
    thread_history_followup_sessions: Vec<Option<String>>,
}

#[derive(Debug, Clone)]
pub struct AppExitInfo {
    pub token_usage: TokenUsage,
    pub thread_id: Option<ThreadId>,
    pub thread_name: Option<String>,
    pub update_action: Option<UpdateAction>,
    pub exit_reason: ExitReason,
}

impl AppExitInfo {
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            token_usage: TokenUsage::default(),
            thread_id: None,
            thread_name: None,
            update_action: None,
            exit_reason: ExitReason::Fatal(message.into()),
        }
    }
}

#[derive(Debug)]
pub(crate) enum AppRunControl {
    Continue,
    Exit(ExitReason),
}

#[derive(Debug, Clone)]
pub enum ExitReason {
    UserRequested,
    Fatal(String),
}

fn session_summary(
    token_usage: TokenUsage,
    thread_id: Option<ThreadId>,
    thread_name: Option<String>,
    rollout_path: Option<&Path>,
) -> Option<SessionSummary> {
    let usage_line = (!token_usage.is_zero()).then(|| FinalOutput::from(token_usage).to_string());
    let thread_id =
        resumable_thread(thread_id, thread_name, rollout_path).map(|thread| thread.thread_id);
    let resume_command =
        crate::legacy_core::util::resume_command(/*thread_name*/ None, thread_id);

    if usage_line.is_none() && resume_command.is_none() {
        return None;
    }

    Some(SessionSummary {
        usage_line,
        resume_command,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResumableThread {
    thread_id: ThreadId,
    thread_name: Option<String>,
}

fn resumable_thread(
    thread_id: Option<ThreadId>,
    thread_name: Option<String>,
    rollout_path: Option<&Path>,
) -> Option<ResumableThread> {
    let thread_id = thread_id?;
    let rollout_path = rollout_path?;
    rollout_path_is_resumable(rollout_path).then_some(ResumableThread {
        thread_id,
        thread_name,
    })
}

fn rollout_path_is_resumable(rollout_path: &Path) -> bool {
    std::fs::metadata(rollout_path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

fn errors_for_cwd(cwd: &Path, response: &ListSkillsResponseEvent) -> Vec<SkillErrorInfo> {
    response
        .skills
        .iter()
        .find(|entry| entry.cwd.as_path() == cwd)
        .map(|entry| entry.errors.clone())
        .unwrap_or_default()
}

fn list_skills_response_to_core(response: SkillsListResponse) -> ListSkillsResponseEvent {
    ListSkillsResponseEvent {
        skills: response
            .data
            .into_iter()
            .map(|entry| codex_protocol::protocol::SkillsListEntry {
                cwd: entry.cwd,
                skills: entry
                    .skills
                    .into_iter()
                    .map(|skill| codex_protocol::protocol::SkillMetadata {
                        name: skill.name,
                        description: skill.description,
                        short_description: skill.short_description,
                        interface: skill.interface.map(|interface| {
                            codex_protocol::protocol::SkillInterface {
                                display_name: interface.display_name,
                                short_description: interface.short_description,
                                icon_small: interface.icon_small,
                                icon_large: interface.icon_large,
                                brand_color: interface.brand_color,
                                default_prompt: interface.default_prompt,
                            }
                        }),
                        dependencies: skill.dependencies.map(|dependencies| {
                            codex_protocol::protocol::SkillDependencies {
                                tools: dependencies
                                    .tools
                                    .into_iter()
                                    .map(|tool| codex_protocol::protocol::SkillToolDependency {
                                        r#type: tool.r#type,
                                        value: tool.value,
                                        description: tool.description,
                                        transport: tool.transport,
                                        command: tool.command,
                                        url: tool.url,
                                    })
                                    .collect(),
                            }
                        }),
                        path: skill.path,
                        scope: match skill.scope {
                            codex_app_server_protocol::SkillScope::User => {
                                codex_protocol::protocol::SkillScope::User
                            }
                            codex_app_server_protocol::SkillScope::Repo => {
                                codex_protocol::protocol::SkillScope::Repo
                            }
                            codex_app_server_protocol::SkillScope::System => {
                                codex_protocol::protocol::SkillScope::System
                            }
                            codex_app_server_protocol::SkillScope::Admin => {
                                codex_protocol::protocol::SkillScope::Admin
                            }
                        },
                        enabled: skill.enabled,
                    })
                    .collect(),
                errors: entry
                    .errors
                    .into_iter()
                    .map(|error| codex_protocol::protocol::SkillErrorInfo {
                        path: error.path,
                        message: error.message,
                    })
                    .collect(),
            })
            .collect(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionSummary {
    usage_line: Option<String>,
    resume_command: Option<String>,
}

#[derive(Debug, Default)]
struct InitialHistoryReplayBuffer {
    retained_lines: VecDeque<Line<'static>>,
}

pub(crate) struct App {
    model_catalog: Arc<ModelCatalog>,
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) chat_widget: ChatWidget,
    /// Config is stored here so we can recreate ChatWidgets as needed.
    pub(crate) config: Config,
    pub(crate) active_profile: Option<String>,
    cli_kv_overrides: Vec<(String, TomlValue)>,
    harness_overrides: ConfigOverrides,
    runtime_approval_policy_override: Option<AskForApproval>,
    runtime_sandbox_policy_override: Option<SandboxPolicy>,

    pub(crate) file_search: FileSearchManager,

    pub(crate) transcript_cells: Vec<Arc<dyn HistoryCell>>,

    // Pager overlay state (Transcript or Static like Diff)
    pub(crate) overlay: Option<Overlay>,
    pub(crate) deferred_history_lines: Vec<Line<'static>>,
    has_emitted_history_lines: bool,
    transcript_reflow: TranscriptReflowState,
    initial_history_replay_buffer: Option<InitialHistoryReplayBuffer>,

    pub(crate) enhanced_keys_supported: bool,

    /// Controls the animation thread that sends CommitTick events.
    pub(crate) commit_anim_running: Arc<AtomicBool>,
    // Shared across ChatWidget instances so invalid status-line config warnings only emit once.
    status_line_invalid_items_warned: Arc<AtomicBool>,
    // Shared across ChatWidget instances so invalid terminal-title config warnings only emit once.
    terminal_title_invalid_items_warned: Arc<AtomicBool>,

    // Esc-backtracking state grouped
    pub(crate) backtrack: crate::app_backtrack::BacktrackState,
    /// When set, the next draw re-renders the transcript into terminal scrollback once.
    ///
    /// This is used after a confirmed thread rollback to ensure scrollback reflects the trimmed
    /// transcript cells.
    pub(crate) backtrack_render_pending: bool,
    pub(crate) feedback: codex_feedback::CodexFeedback,
    feedback_audience: FeedbackAudience,
    environment_manager: Arc<EnvironmentManager>,
    remote_app_server_url: Option<String>,
    remote_app_server_auth_token: Option<String>,
    exit_after_turn: bool,
    exit_after_turn_observed_assistant_output: bool,
    exit_after_turn_thread_id: Option<String>,
    exit_after_turn_turn_id: Option<String>,
    /// Set when the user confirms an update; propagated on exit.
    pub(crate) pending_update_action: Option<UpdateAction>,

    /// Tracks the thread we intentionally shut down while exiting the app.
    ///
    /// When this matches the active thread, its `ShutdownComplete` should lead to
    /// process exit instead of being treated as an unexpected sub-agent death that
    /// triggers failover to the primary thread.
    ///
    /// This is thread-scoped state (`Option<ThreadId>`) instead of a global bool
    /// so shutdown events from other threads still take the normal failover path.
    pending_shutdown_exit_thread_id: Option<ThreadId>,

    windows_sandbox: WindowsSandboxState,

    thread_event_channels: HashMap<ThreadId, ThreadEventChannel>,
    thread_event_listener_tasks: HashMap<ThreadId, JoinHandle<()>>,
    agent_navigation: AgentNavigationState,
    side_threads: HashMap<ThreadId, SideThreadState>,
    active_thread_id: Option<ThreadId>,
    active_thread_rx: Option<mpsc::Receiver<ThreadBufferedEvent>>,
    primary_thread_id: Option<ThreadId>,
    last_subagent_backfill_attempt: Option<ThreadId>,
    primary_session_configured: Option<ThreadSessionState>,
    pending_primary_events: VecDeque<ThreadBufferedEvent>,
    pending_app_server_requests: PendingAppServerRequests,
    pending_startup_ui_action: Option<StartupUiAction>,
    oracle_state: OracleSupervisorState,
    oracle_picker_show_info: bool,
    oracle_picker_remote_threads: Vec<OracleBrokerThreadEntry>,
    oracle_picker_include_new_thread: bool,
    oracle_picker_remote_list_request_id: u64,
    oracle_picker_remote_list_pending: bool,
    oracle_picker_remote_list_tick: usize,
    oracle_picker_remote_list_notice: Option<String>,
    oracle_broker: Option<(PathBuf, OracleBrokerClient)>,
    #[cfg(test)]
    oracle_test_broker_hooks: Arc<StdMutex<OracleTestBrokerHooks>>,
    // Serialize plugin enablement writes per plugin so stale completions cannot
    // overwrite a newer toggle, even if the plugin is toggled from different
    // cwd contexts.
    pending_plugin_enabled_writes: HashMap<String, Option<bool>>,
}

#[derive(Default)]
struct WindowsSandboxState {
    setup_started_at: Option<Instant>,
    // One-shot suppression of the next world-writable scan after user confirmation.
    skip_world_writable_scan_once: bool,
}

fn active_turn_not_steerable_turn_error(error: &TypedRequestError) -> Option<AppServerTurnError> {
    let TypedRequestError::Server { source, .. } = error else {
        return None;
    };
    let turn_error: AppServerTurnError = serde_json::from_value(source.data.clone()?).ok()?;
    matches!(
        turn_error.codex_error_info,
        Some(AppServerCodexErrorInfo::ActiveTurnNotSteerable { .. })
    )
    .then_some(turn_error)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActiveTurnSteerRace {
    Missing,
    ExpectedTurnMismatch { actual_turn_id: String },
}

fn active_turn_steer_race(error: &TypedRequestError) -> Option<ActiveTurnSteerRace> {
    let TypedRequestError::Server { method, source } = error else {
        return None;
    };
    if method != "turn/steer" {
        return None;
    }
    if source.message == "no active turn to steer" {
        return Some(ActiveTurnSteerRace::Missing);
    }

    // App-server steer mismatches mean our cached active turn id is stale, but the response
    // includes the server's current active turn so we can resynchronize and retry once.
    let mismatch_prefix = "expected active turn id `";
    let mismatch_separator = "` but found `";
    let actual_turn_id = source
        .message
        .strip_prefix(mismatch_prefix)?
        .split_once(mismatch_separator)?
        .1
        .strip_suffix('`')?
        .to_string();
    Some(ActiveTurnSteerRace::ExpectedTurnMismatch { actual_turn_id })
}

impl App {
    pub fn chatwidget_init_for_forked_or_resumed_thread(
        &self,
        tui: &mut tui::Tui,
        cfg: crate::legacy_core::config::Config,
        initial_user_message: Option<crate::chatwidget::UserMessage>,
    ) -> crate::chatwidget::ChatWidgetInit {
        crate::chatwidget::ChatWidgetInit {
            config: cfg,
            frame_requester: tui.frame_requester(),
            app_event_tx: self.app_event_tx.clone(),
            initial_user_message,
            enhanced_keys_supported: self.enhanced_keys_supported,
            has_chatgpt_account: self.chat_widget.has_chatgpt_account(),
            model_catalog: self.model_catalog.clone(),
            feedback: self.feedback.clone(),
            is_first_run: false,
            status_account_display: self.chat_widget.status_account_display().cloned(),
            initial_plan_type: self.chat_widget.current_plan_type(),
            model: Some(self.chat_widget.current_model().to_string()),
            startup_tooltip_override: None,
            status_line_invalid_items_warned: self.status_line_invalid_items_warned.clone(),
            terminal_title_invalid_items_warned: self.terminal_title_invalid_items_warned.clone(),
            session_telemetry: self.session_telemetry.clone(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        tui: &mut tui::Tui,
        mut app_server: AppServerSession,
        mut config: Config,
        cli_kv_overrides: Vec<(String, TomlValue)>,
        harness_overrides: ConfigOverrides,
        active_profile: Option<String>,
        initial_prompt: Option<String>,
        initial_images: Vec<PathBuf>,
        session_selection: SessionSelection,
        feedback: codex_feedback::CodexFeedback,
        is_first_run: bool,
        entered_trust_nux: bool,
        should_prompt_windows_sandbox_nux_at_startup: bool,
        remote_app_server_url: Option<String>,
        remote_app_server_auth_token: Option<String>,
        exit_after_turn: bool,
        environment_manager: Arc<EnvironmentManager>,
    ) -> Result<AppExitInfo> {
        use tokio_stream::StreamExt;
        let (app_event_tx, mut app_event_rx) = unbounded_channel();
        let app_event_tx = AppEventSender::new(app_event_tx);
        emit_project_config_warnings(&app_event_tx, &config);
        emit_system_bwrap_warning(&app_event_tx, &config);
        tui.set_notification_settings(
            config.tui_notifications.method,
            config.tui_notifications.condition,
        );

        let harness_overrides =
            normalize_harness_overrides_for_cwd(harness_overrides, &config.cwd)?;
        let external_agent_config_migration_outcome =
            handle_external_agent_config_migration_prompt_if_needed(
                tui,
                &mut app_server,
                &mut config,
                &cli_kv_overrides,
                &harness_overrides,
                entered_trust_nux,
            )
            .await?;
        let external_agent_config_migration_message = match external_agent_config_migration_outcome
        {
            ExternalAgentConfigMigrationStartupOutcome::Continue { success_message } => {
                success_message
            }
            ExternalAgentConfigMigrationStartupOutcome::ExitRequested => {
                app_server
                    .shutdown()
                    .await
                    .inspect_err(|err| {
                        tracing::warn!("app-server shutdown failed: {err}");
                    })
                    .ok();
                return Ok(AppExitInfo {
                    token_usage: TokenUsage::default(),
                    thread_id: None,
                    thread_name: None,
                    update_action: None,
                    exit_reason: ExitReason::UserRequested,
                });
            }
        };
        let bootstrap = app_server.bootstrap(&config).await?;
        let mut model = bootstrap.default_model;
        let available_models = bootstrap.available_models;
        let exit_info = handle_model_migration_prompt_if_needed(
            tui,
            &mut config,
            model.as_str(),
            &app_event_tx,
            &available_models,
        )
        .await;
        if let Some(exit_info) = exit_info {
            app_server
                .shutdown()
                .await
                .inspect_err(|err| {
                    tracing::warn!("app-server shutdown failed: {err}");
                })
                .ok();
            return Ok(exit_info);
        }
        if let Some(updated_model) = config.model.clone() {
            model = updated_model;
        }
        let model_catalog = Arc::new(ModelCatalog::new(
            available_models.clone(),
            CollaborationModesConfig {
                default_mode_request_user_input: config
                    .features
                    .enabled(Feature::DefaultModeRequestUserInput),
            },
        ));
        let feedback_audience = bootstrap.feedback_audience;
        let auth_mode = bootstrap.auth_mode;
        let has_chatgpt_account = bootstrap.has_chatgpt_account;
        let requires_openai_auth = bootstrap.requires_openai_auth;
        let status_account_display = bootstrap.status_account_display.clone();
        let initial_plan_type = bootstrap.plan_type;
        let session_telemetry = SessionTelemetry::new(
            ThreadId::new(),
            model.as_str(),
            model.as_str(),
            /*account_id*/ None,
            bootstrap.account_email.clone(),
            auth_mode,
            codex_login::default_client::originator().value,
            config.otel.log_user_prompt,
            user_agent(),
            SessionSource::Cli,
        );
        if config
            .tui_status_line
            .as_ref()
            .is_some_and(|cmd| !cmd.is_empty())
        {
            session_telemetry.counter("codex.status_line", /*inc*/ 1, &[]);
        }

        let status_line_invalid_items_warned = Arc::new(AtomicBool::new(false));
        let terminal_title_invalid_items_warned = Arc::new(AtomicBool::new(false));

        let enhanced_keys_supported = tui.enhanced_keys_supported();
        let wait_for_initial_session_configured =
            Self::should_wait_for_initial_session(&session_selection);
        let (mut chat_widget, initial_started_thread) = match session_selection {
            SessionSelection::StartFresh | SessionSelection::Exit => {
                let started = app_server.start_thread(&config).await?;
                let startup_tooltip_override =
                    prepare_startup_tooltip_override(&mut config, &available_models, is_first_run)
                        .await;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    has_chatgpt_account,
                    model_catalog: model_catalog.clone(),
                    feedback: feedback.clone(),
                    is_first_run,
                    status_account_display: status_account_display.clone(),
                    initial_plan_type,
                    model: Some(model.clone()),
                    startup_tooltip_override,
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    terminal_title_invalid_items_warned: terminal_title_invalid_items_warned
                        .clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                (ChatWidget::new_with_app_event(init), Some(started))
            }
            SessionSelection::Resume(target_session) => {
                let resumed = app_server
                    .resume_thread(config.clone(), target_session.thread_id)
                    .await
                    .wrap_err_with(|| {
                        let target_label = target_session.display_label();
                        format!("Failed to resume session from {target_label}")
                    })?;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    has_chatgpt_account,
                    model_catalog: model_catalog.clone(),
                    feedback: feedback.clone(),
                    is_first_run,
                    status_account_display: status_account_display.clone(),
                    initial_plan_type,
                    model: config.model.clone(),
                    startup_tooltip_override: None,
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    terminal_title_invalid_items_warned: terminal_title_invalid_items_warned
                        .clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                (ChatWidget::new_with_app_event(init), Some(resumed))
            }
            SessionSelection::Fork(target_session) => {
                session_telemetry.counter(
                    "codex.thread.fork",
                    /*inc*/ 1,
                    &[("source", "cli_subcommand")],
                );
                let forked = app_server
                    .fork_thread(config.clone(), target_session.thread_id)
                    .await
                    .wrap_err_with(|| {
                        let target_label = target_session.display_label();
                        format!("Failed to fork session from {target_label}")
                    })?;
                let init = crate::chatwidget::ChatWidgetInit {
                    config: config.clone(),
                    frame_requester: tui.frame_requester(),
                    app_event_tx: app_event_tx.clone(),
                    initial_user_message: crate::chatwidget::create_initial_user_message(
                        initial_prompt.clone(),
                        initial_images.clone(),
                        // CLI prompt args are plain strings, so they don't provide element ranges.
                        Vec::new(),
                    ),
                    enhanced_keys_supported,
                    has_chatgpt_account,
                    model_catalog: model_catalog.clone(),
                    feedback: feedback.clone(),
                    is_first_run,
                    status_account_display: status_account_display.clone(),
                    initial_plan_type,
                    model: config.model.clone(),
                    startup_tooltip_override: None,
                    status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
                    terminal_title_invalid_items_warned: terminal_title_invalid_items_warned
                        .clone(),
                    session_telemetry: session_telemetry.clone(),
                };
                (ChatWidget::new_with_app_event(init), Some(forked))
            }
        };
        if let Some(message) = external_agent_config_migration_message {
            chat_widget.add_info_message(message, /*hint*/ None);
        }

        chat_widget
            .maybe_prompt_windows_sandbox_enable(should_prompt_windows_sandbox_nux_at_startup);

        let file_search = FileSearchManager::new(config.cwd.to_path_buf(), app_event_tx.clone());
        #[cfg(not(debug_assertions))]
        let upgrade_version = crate::updates::get_upgrade_version(&config);

        let mut app = Self {
            model_catalog,
            session_telemetry: session_telemetry.clone(),
            app_event_tx,
            chat_widget,
            config,
            active_profile,
            cli_kv_overrides,
            harness_overrides,
            runtime_approval_policy_override: None,
            runtime_sandbox_policy_override: None,
            file_search,
            enhanced_keys_supported,
            transcript_cells: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            transcript_reflow: TranscriptReflowState::default(),
            initial_history_replay_buffer: None,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            status_line_invalid_items_warned: status_line_invalid_items_warned.clone(),
            terminal_title_invalid_items_warned: terminal_title_invalid_items_warned.clone(),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            feedback: feedback.clone(),
            feedback_audience,
            environment_manager,
            remote_app_server_url,
            remote_app_server_auth_token,
            exit_after_turn,
            exit_after_turn_observed_assistant_output: false,
            exit_after_turn_thread_id: None,
            exit_after_turn_turn_id: None,
            pending_update_action: None,
            pending_shutdown_exit_thread_id: None,
            windows_sandbox: WindowsSandboxState::default(),
            thread_event_channels: HashMap::new(),
            thread_event_listener_tasks: HashMap::new(),
            agent_navigation: AgentNavigationState::default(),
            side_threads: HashMap::new(),
            active_thread_id: None,
            active_thread_rx: None,
            primary_thread_id: None,
            last_subagent_backfill_attempt: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
            pending_app_server_requests: PendingAppServerRequests::default(),
            pending_startup_ui_action: StartupUiAction::from_env(),
            oracle_state: OracleSupervisorState::default(),
            oracle_picker_show_info: false,
            oracle_picker_remote_threads: Vec::new(),
            oracle_picker_include_new_thread: false,
            oracle_picker_remote_list_request_id: 0,
            oracle_picker_remote_list_pending: false,
            oracle_picker_remote_list_tick: 0,
            oracle_picker_remote_list_notice: None,
            oracle_broker: None,
            #[cfg(test)]
            oracle_test_broker_hooks: Arc::new(StdMutex::new(OracleTestBrokerHooks::default())),
            pending_plugin_enabled_writes: HashMap::new(),
        };
        if let Some(started) = initial_started_thread {
            app.enqueue_primary_thread_session(started.session, started.turns)
                .await?;
        }

        // On startup, if Agent mode (workspace-write) or ReadOnly is active, warn about world-writable dirs on Windows.
        #[cfg(target_os = "windows")]
        {
            let startup_sandbox_policy = app
                .config
                .permissions
                .legacy_sandbox_policy(app.config.cwd.as_path());
            let should_check = WindowsSandboxLevel::from_config(&app.config)
                != WindowsSandboxLevel::Disabled
                && matches!(
                    &startup_sandbox_policy,
                    codex_protocol::protocol::SandboxPolicy::WorkspaceWrite { .. }
                        | codex_protocol::protocol::SandboxPolicy::ReadOnly { .. }
                )
                && !app
                    .config
                    .notices
                    .hide_world_writable_warning
                    .unwrap_or(false);
            if should_check {
                let cwd = app.config.cwd.clone();
                let env_map: std::collections::HashMap<String, String> = std::env::vars().collect();
                let tx = app.app_event_tx.clone();
                let logs_base_dir = app.config.codex_home.clone();
                let sandbox_policy = startup_sandbox_policy;
                Self::spawn_world_writable_scan(cwd, env_map, logs_base_dir, sandbox_policy, tx);
            }
        }

        let tui_events = tui.event_stream();
        tokio::pin!(tui_events);

        tui.frame_requester().schedule_frame();
        app.refresh_startup_skills(&app_server);
        // Kick off a non-blocking rate-limit prefetch so the first `/status`
        // already has data, without delaying the initial frame render.
        if requires_openai_auth && has_chatgpt_account {
            app.refresh_rate_limits(&app_server, RateLimitRefreshOrigin::StartupPrefetch);
        }

        let mut listen_for_app_server_events = true;
        let mut waiting_for_initial_session_configured = wait_for_initial_session_configured;

        #[cfg(not(debug_assertions))]
        let pre_loop_exit_reason = if let Some(latest_version) = upgrade_version {
            let control = app
                .handle_event(
                    tui,
                    &mut app_server,
                    AppEvent::InsertHistoryCell(Box::new(UpdateAvailableHistoryCell::new(
                        latest_version,
                        crate::update_action::get_update_action(),
                    ))),
                )
                .await?;
            match control {
                AppRunControl::Continue => None,
                AppRunControl::Exit(exit_reason) => Some(exit_reason),
            }
        } else {
            None
        };
        #[cfg(debug_assertions)]
        let pre_loop_exit_reason: Option<ExitReason> = None;

        let exit_reason_result = if let Some(exit_reason) = pre_loop_exit_reason {
            Ok(exit_reason)
        } else {
            loop {
                let control = select! {
                    Some(event) = app_event_rx.recv() => {
                        match app.handle_event(tui, &mut app_server, event).await {
                            Ok(control) => control,
                            Err(err) => break Err(err),
                        }
                    }
                    active = async {
                        if let Some(rx) = app.active_thread_rx.as_mut() {
                            rx.recv().await
                        } else {
                            None
                        }
                    }, if App::should_handle_active_thread_events(
                        waiting_for_initial_session_configured,
                        app.active_thread_rx.is_some()
                    ) => {
                        if let Some(event) = active {
                            let exit_after_turn_event = event.clone();
                            if let Err(err) = app.handle_active_thread_event(tui, &mut app_server, event).await {
                                break Err(err);
                            }
                            if let Some(exit_reason) = app.exit_after_turn_control_for_event(&exit_after_turn_event) {
                                break Ok(exit_reason);
                            }
                        } else {
                            app.clear_active_thread().await;
                        }
                        AppRunControl::Continue
                    }
                    _ = tokio::time::sleep(EXIT_AFTER_TURN_TIMEOUT), if app.exit_after_turn => {
                        AppRunControl::Exit(ExitReason::Fatal(
                            "Remote turn did not complete before --exit-after-turn timeout.".to_string(),
                        ))
                    }
                    event = tui_events.next() => {
                        if let Some(event) = event {
                            match app.handle_tui_event(tui, &mut app_server, event).await {
                                Ok(control) => control,
                                Err(err) => break Err(err),
                            }
                        } else {
                            tracing::warn!("terminal input stream closed; shutting down active thread");
                            app.handle_exit_mode(&mut app_server, ExitMode::ShutdownFirst).await
                        }
                    }
                    app_server_event = app_server.next_event(), if listen_for_app_server_events => {
                        match app_server_event {
                            Some(event) => app.handle_app_server_event(&app_server, event).await,
                            None => {
                                listen_for_app_server_events = false;
                                tracing::warn!("app-server event stream closed");
                            }
                        }
                        AppRunControl::Continue
                    }
                };
                if App::should_stop_waiting_for_initial_session(
                    waiting_for_initial_session_configured,
                    app.primary_thread_id,
                ) {
                    waiting_for_initial_session_configured = false;
                }
                match control {
                    AppRunControl::Continue => {}
                    AppRunControl::Exit(reason) => break Ok(reason),
                }
            }
        };
        if let Err(err) = app_server.shutdown().await {
            tracing::warn!(error = %err, "failed to shut down embedded app server");
        }
        let clear_result = tui.terminal.clear();
        let exit_reason = match exit_reason_result {
            Ok(exit_reason) => {
                clear_result?;
                exit_reason
            }
            Err(err) => {
                if let Err(clear_err) = clear_result {
                    tracing::warn!(error = %clear_err, "failed to clear terminal UI");
                }
                return Err(err);
            }
        };
        let resumable_thread = resumable_thread(
            app.chat_widget.thread_id(),
            app.chat_widget.thread_name(),
            app.chat_widget.rollout_path().as_deref(),
        );
        Ok(AppExitInfo {
            token_usage: app.token_usage(),
            thread_id: resumable_thread.as_ref().map(|thread| thread.thread_id),
            thread_name: resumable_thread.and_then(|thread| thread.thread_name),
            update_action: app.pending_update_action,
            exit_reason,
        })
    }
}

impl App {
    fn persist_active_oracle_binding(&mut self) {
        let Some(thread_id) = self.oracle_state.oracle_thread_id else {
            return;
        };
        let existing = self.oracle_state.bindings.get(&thread_id).cloned();
        let mut binding = OracleThreadBinding {
            session_root_slug: self.oracle_state.session_root_slug.clone(),
            current_session_id: self.oracle_state.current_session_id.clone(),
            current_session_ownership: self.oracle_state.current_session_ownership.clone(),
            current_session_verified: existing
                .as_ref()
                .is_some_and(|existing| existing.current_session_verified),
            orchestrator_thread_id: self.oracle_state.orchestrator_thread_id,
            workflow: self.oracle_state.workflow.clone(),
            phase: self.oracle_state.phase,
            active_run_id: self
                .oracle_state
                .active_run_id
                .clone()
                .or_else(|| self.oracle_state.inflight_runs.get(&thread_id).cloned()),
            aborted_run_id: self.oracle_state.aborted_run_id.clone(),
            last_status: self.oracle_state.last_status.clone(),
            last_orchestrator_task: self.oracle_state.last_orchestrator_task.clone(),
            pending_turn_id: self.oracle_state.pending_turn_id.clone(),
            automatic_context_followups: self.oracle_state.automatic_context_followups,
            accept_user_turn_workflow_replacement: self
                .oracle_state
                .accept_user_turn_workflow_replacement,
            conversation_id: existing
                .as_ref()
                .and_then(|existing| existing.conversation_id.clone()),
            remote_title: existing
                .as_ref()
                .and_then(|existing| existing.remote_title.clone()),
            participants: existing
                .as_ref()
                .map(|existing| existing.participants.clone())
                .unwrap_or_default(),
            pending_checkpoint_threads: self
                .oracle_state
                .bindings
                .get(&thread_id)
                .map(|existing| existing.pending_checkpoint_threads.clone())
                .unwrap_or_default(),
            pending_checkpoint_versions: self
                .oracle_state
                .bindings
                .get(&thread_id)
                .map(|existing| existing.pending_checkpoint_versions.clone())
                .unwrap_or_default(),
            pending_checkpoint_turn_ids: self
                .oracle_state
                .bindings
                .get(&thread_id)
                .map(|existing| existing.pending_checkpoint_turn_ids.clone())
                .unwrap_or_default(),
            last_checkpoint_turn_ids: self
                .oracle_state
                .bindings
                .get(&thread_id)
                .map(|existing| existing.last_checkpoint_turn_ids.clone())
                .unwrap_or_default(),
            active_checkpoint_thread_id: self.oracle_state.active_checkpoint_thread_id,
            active_checkpoint_turn_id: self.oracle_state.active_checkpoint_turn_id.clone(),
        };
        Self::sync_legacy_oracle_binding_fields(&mut binding);
        self.oracle_state.bindings.insert(thread_id, binding);
    }

    fn activate_oracle_binding(&mut self, thread_id: ThreadId) {
        self.persist_active_oracle_binding();
        self.oracle_state
            .bindings
            .entry(thread_id)
            .or_insert_with(|| OracleThreadBinding {
                phase: OracleSupervisorPhase::Idle,
                ..Default::default()
            });
        if let Some(binding) = self.oracle_state.bindings.get_mut(&thread_id) {
            if binding.active_run_id.is_none() {
                binding.active_run_id = self.oracle_state.inflight_runs.get(&thread_id).cloned();
            }
            Self::sync_legacy_oracle_binding_fields(binding);
        }
        let Some(binding) = self.oracle_state.bindings.get(&thread_id).cloned() else {
            return;
        };
        self.oracle_state.oracle_thread_id = Some(thread_id);
        self.oracle_state.session_root_slug = binding.session_root_slug;
        self.oracle_state.current_session_id = binding.current_session_id;
        self.oracle_state.current_session_ownership = binding.current_session_ownership;
        self.oracle_state.orchestrator_thread_id = binding.orchestrator_thread_id;
        self.oracle_state.workflow = binding.workflow;
        self.oracle_state.phase = binding.phase;
        self.oracle_state.active_run_id = binding
            .active_run_id
            .or_else(|| self.oracle_state.inflight_runs.get(&thread_id).cloned());
        self.oracle_state.aborted_run_id = binding.aborted_run_id;
        self.oracle_state.last_status = binding.last_status;
        self.oracle_state.last_orchestrator_task = binding.last_orchestrator_task;
        self.oracle_state.pending_turn_id = binding.pending_turn_id;
        self.oracle_state.automatic_context_followups = binding.automatic_context_followups;
        self.oracle_state.accept_user_turn_workflow_replacement =
            binding.accept_user_turn_workflow_replacement;
        self.oracle_state.active_checkpoint_thread_id = binding.active_checkpoint_thread_id;
        self.oracle_state.active_checkpoint_turn_id = binding.active_checkpoint_turn_id;
    }

    fn set_oracle_remote_metadata(
        &mut self,
        thread_id: ThreadId,
        conversation_id: Option<String>,
        remote_title: Option<String>,
    ) {
        let binding = self
            .oracle_state
            .bindings
            .entry(thread_id)
            .or_insert_with(|| OracleThreadBinding {
                phase: OracleSupervisorPhase::Idle,
                ..Default::default()
            });
        binding.conversation_id = conversation_id;
        if let Some(title) = remote_title {
            self.oracle_state
                .bindings
                .entry(thread_id)
                .or_insert_with(|| OracleThreadBinding {
                    phase: OracleSupervisorPhase::Idle,
                    ..Default::default()
                })
                .remote_title = Some(title);
        }
    }

    fn merge_oracle_remote_metadata(
        &mut self,
        thread_id: ThreadId,
        conversation_id: Option<String>,
        remote_title: Option<String>,
    ) {
        let binding = self
            .oracle_state
            .bindings
            .entry(thread_id)
            .or_insert_with(|| OracleThreadBinding {
                phase: OracleSupervisorPhase::Idle,
                ..Default::default()
            });
        let conversation_matches = conversation_id.as_deref().is_none_or(|incoming| {
            binding
                .conversation_id
                .as_deref()
                .is_none_or(|existing| existing == incoming)
        });
        if conversation_matches && let Some(conversation_id) = conversation_id {
            binding.conversation_id = Some(conversation_id);
        }
        if conversation_matches
            && let Some(title) = remote_title.filter(|title| !title.trim().is_empty())
        {
            binding.remote_title = Some(title);
        }
    }

    fn oracle_remote_metadata_identity_error(
        &self,
        thread_id: ThreadId,
        conversation_id: Option<&str>,
    ) -> Option<String> {
        let incoming = conversation_id
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let existing = self
            .oracle_state
            .bindings
            .get(&thread_id)
            .and_then(|binding| binding.conversation_id.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match (existing, incoming) {
            (Some(expected), Some(observed)) if expected != observed => Some(format!(
                "Oracle thread identity violation: local thread {thread_id} is bound to remote conversation {expected}, but Oracle reported {observed}. Refusing to continue on the wrong Oracle thread."
            )),
            (Some(expected), None) => Some(format!(
                "Oracle thread identity violation: local thread {thread_id} is bound to remote conversation {expected}, but Oracle omitted conversation metadata. Refusing to continue on an unverified Oracle thread."
            )),
            _ => None,
        }
    }

    fn generate_oracle_workflow_id(thread_id: ThreadId) -> String {
        let short: String = thread_id.to_string().chars().take(8).collect();
        let suffix = Uuid::new_v4().simple().to_string();
        format!("oracle-wf-{short}-{}", &suffix[..8])
    }

    fn ensure_active_oracle_workflow(
        &mut self,
        workflow_id_hint: Option<String>,
        objective_hint: Option<String>,
        summary_hint: Option<String>,
        version_hint: Option<u64>,
    ) -> OracleWorkflowBinding {
        let oracle_thread_id = self.oracle_state.oracle_thread_id.unwrap_or_default();
        let requested_workflow_id = workflow_id_hint.unwrap_or_else(|| {
            self.oracle_state
                .workflow
                .as_ref()
                .filter(|workflow| matches!(workflow.status, OracleWorkflowStatus::Running))
                .map(|workflow| workflow.workflow_id.clone())
                .unwrap_or_else(|| Self::generate_oracle_workflow_id(oracle_thread_id))
        });
        let requested_version = version_hint.unwrap_or(1).max(1);
        let replace_workflow = self.oracle_state.workflow.as_ref().is_none_or(|workflow| {
            workflow.workflow_id != requested_workflow_id
                || !matches!(workflow.status, OracleWorkflowStatus::Running)
        });
        let workflow = self
            .oracle_state
            .workflow
            .get_or_insert_with(|| OracleWorkflowBinding {
                workflow_id: requested_workflow_id.clone(),
                mode: OracleWorkflowMode::Supervising,
                status: OracleWorkflowStatus::Running,
                version: requested_version,
                ..Default::default()
            });
        if replace_workflow {
            *workflow = OracleWorkflowBinding {
                workflow_id: requested_workflow_id,
                mode: OracleWorkflowMode::Supervising,
                status: OracleWorkflowStatus::Running,
                version: requested_version,
                ..Default::default()
            };
        }
        workflow.mode = OracleWorkflowMode::Supervising;
        workflow.status = OracleWorkflowStatus::Running;
        if replace_workflow {
            workflow.version = requested_version;
        } else {
            workflow.version = workflow.version.max(1);
        }
        if let Some(objective) = objective_hint.filter(|value| !value.trim().is_empty()) {
            workflow.objective = Some(objective);
        } else if workflow.objective.is_none() {
            workflow.objective = self.oracle_state.last_orchestrator_task.clone();
        }
        if let Some(summary) = summary_hint.filter(|value| !value.trim().is_empty()) {
            workflow.summary = Some(summary);
        }
        workflow.last_blocker = None;
        workflow.orchestrator_thread_id = self.oracle_state.orchestrator_thread_id;
        workflow.clone()
    }

    fn current_oracle_workflow_status_line(binding: &OracleThreadBinding) -> String {
        binding.workflow.as_ref().map_or_else(
            || "Chat | no workflow".to_string(),
            |workflow| {
                format!(
                    "Supervising | wf:{} | {} | v{}",
                    workflow.workflow_id,
                    workflow.status.label(),
                    workflow.version
                )
            },
        )
    }

    fn active_oracle_workflow_stale_reason(
        &self,
        directive: Option<&OracleControlDirective>,
    ) -> Option<String> {
        let directive = directive?;
        let workflow = self.oracle_state.workflow.as_ref()?;
        if matches!(workflow.status, OracleWorkflowStatus::Running) {
            if let Some(workflow_id) = directive.workflow_id.as_deref()
                && workflow_id != workflow.workflow_id
            {
                return Some(format!(
                    "Oracle replied for workflow `{workflow_id}`, but the active workflow is `{}`. Ignoring the stale response.",
                    workflow.workflow_id
                ));
            }
            if let Some(version) = directive.workflow_version
                && version != workflow.version
            {
                return Some(format!(
                    "Oracle replied with workflow version {version} for `{}` while the active workflow is at version {}. Ignoring the stale response.",
                    workflow.workflow_id, workflow.version
                ));
            }
            return None;
        }
        if let Some(workflow_id) = directive.workflow_id.as_deref()
            && workflow_id == workflow.workflow_id
        {
            return Some(format!(
                "Oracle replied for terminal workflow `{workflow_id}` after that workflow had already completed. Ignoring the stale response."
            ));
        }
        if !self.oracle_state.accept_user_turn_workflow_replacement {
            return None;
        }
        None
    }

    fn detach_terminal_oracle_workflow_for_new_user_turn_if_needed(
        &mut self,
        action: OracleAction,
        directive: Option<&OracleControlDirective>,
    ) {
        let oracle_thread_id = self.oracle_state.oracle_thread_id;
        let should_detach = self.oracle_state.accept_user_turn_workflow_replacement
            && self.oracle_state.workflow.as_ref().is_some_and(|workflow| {
                if !matches!(
                    workflow.status,
                    OracleWorkflowStatus::Complete | OracleWorkflowStatus::Failed
                ) {
                    return false;
                }
                let requested_workflow_id =
                    directive.and_then(|directive| directive.workflow_id.as_deref());
                if requested_workflow_id
                    .is_some_and(|workflow_id| workflow_id == workflow.workflow_id)
                {
                    return false;
                }
                let starts_valid_replacement = requested_workflow_id.is_some_and(|workflow_id| {
                    workflow_id != workflow.workflow_id && !matches!(action, OracleAction::Reply)
                });
                !starts_valid_replacement
            });
        if !should_detach {
            return;
        }
        self.oracle_state.workflow = None;
        if let Some(oracle_thread_id) = oracle_thread_id {
            self.clear_oracle_orchestrator_binding(oracle_thread_id);
        } else {
            self.oracle_state.orchestrator_thread_id = None;
        }
        self.oracle_state.accept_user_turn_workflow_replacement = false;
    }

    fn reopen_active_oracle_workflow_for_user_turn(&mut self) {
        let Some(workflow) = self.oracle_state.workflow.as_mut() else {
            self.oracle_state.accept_user_turn_workflow_replacement = false;
            return;
        };
        if matches!(workflow.status, OracleWorkflowStatus::Running) {
            self.oracle_state.accept_user_turn_workflow_replacement = false;
            return;
        }
        if matches!(
            workflow.status,
            OracleWorkflowStatus::Complete | OracleWorkflowStatus::Failed
        ) {
            self.oracle_state.accept_user_turn_workflow_replacement = true;
            return;
        }
        self.oracle_state.accept_user_turn_workflow_replacement = false;
        workflow.mode = OracleWorkflowMode::Supervising;
        workflow.status = OracleWorkflowStatus::Running;
        workflow.version = workflow.version.saturating_add(1).max(1);
        workflow.pending_op_ids.clear();
        workflow.pending_idempotency_keys.clear();
        workflow.applied_op_ids.clear();
        workflow.applied_idempotency_keys.clear();
        workflow.last_blocker = None;
        workflow.orchestrator_thread_id = self.oracle_state.orchestrator_thread_id;
    }

    fn oracle_workflow_requires_control(&self) -> bool {
        self.oracle_state.workflow.as_ref().is_some_and(|workflow| {
            !matches!(
                workflow.status,
                OracleWorkflowStatus::Complete | OracleWorkflowStatus::Failed
            )
        })
    }

    fn adopt_replacement_oracle_workflow_if_allowed(
        &mut self,
        kind: OracleRequestKind,
        action: OracleAction,
        directive: Option<&OracleControlDirective>,
    ) {
        if kind != OracleRequestKind::UserTurn
            || !self.oracle_state.accept_user_turn_workflow_replacement
        {
            return;
        }
        let Some(directive) = directive else {
            return;
        };
        let Some(workflow_id) = directive
            .workflow_id
            .as_deref()
            .filter(|workflow_id| !workflow_id.trim().is_empty())
        else {
            return;
        };
        let should_replace = self
            .oracle_state
            .workflow
            .as_ref()
            .is_some_and(|workflow| workflow.workflow_id != workflow_id);
        if !should_replace {
            return;
        }
        let oracle_thread_id = self.oracle_state.oracle_thread_id;
        self.oracle_state.accept_user_turn_workflow_replacement = false;
        if let Some(oracle_thread_id) = oracle_thread_id {
            self.clear_oracle_orchestrator_binding(oracle_thread_id);
        } else {
            self.oracle_state.orchestrator_thread_id = None;
        }
        self.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: workflow_id.to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: Self::oracle_workflow_status_for_action(
                action,
                directive.status.as_deref(),
                OracleWorkflowStatus::Running,
            ),
            version: directive.workflow_version.unwrap_or(1).max(1),
            objective: directive.objective.clone(),
            summary: directive.summary.clone(),
            orchestrator_thread_id: None,
            ..Default::default()
        });
    }

    fn update_active_oracle_workflow_status(
        &mut self,
        status: OracleWorkflowStatus,
        summary: Option<String>,
        blocker: Option<String>,
    ) {
        let Some(workflow) = self.oracle_state.workflow.as_mut() else {
            return;
        };
        workflow.status = status;
        if let Some(summary) = summary.filter(|value| !value.trim().is_empty()) {
            workflow.summary = Some(summary);
            if !matches!(
                status,
                OracleWorkflowStatus::Failed | OracleWorkflowStatus::NeedsHuman
            ) && blocker.as_ref().is_none_or(|value| value.trim().is_empty())
            {
                workflow.last_checkpoint = workflow.summary.clone();
            }
        }
        workflow.last_blocker = blocker.filter(|value| !value.trim().is_empty());
        workflow.orchestrator_thread_id = self.oracle_state.orchestrator_thread_id;
    }

    fn oracle_workflow_status_from_hint(
        status_hint: Option<&str>,
        fallback: OracleWorkflowStatus,
    ) -> OracleWorkflowStatus {
        match status_hint
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("idle") => OracleWorkflowStatus::Idle,
            Some("running") | Some("working") | Some("in_progress") => {
                OracleWorkflowStatus::Running
            }
            Some("needs_human") | Some("needs_input") | Some("needs_you") => {
                OracleWorkflowStatus::NeedsHuman
            }
            Some("checkpoint") | Some("milestone") | Some("continuing") => {
                OracleWorkflowStatus::Running
            }
            Some("complete") | Some("completed") | Some("done") => OracleWorkflowStatus::Complete,
            Some("failed") | Some("blocked") | Some("error") => OracleWorkflowStatus::Failed,
            _ => fallback,
        }
    }

    fn oracle_workflow_status_for_action(
        action: OracleAction,
        status_hint: Option<&str>,
        fallback: OracleWorkflowStatus,
    ) -> OracleWorkflowStatus {
        let hinted = Self::oracle_workflow_status_from_hint(status_hint, fallback);
        match action {
            OracleAction::Finish => match hinted {
                OracleWorkflowStatus::Complete | OracleWorkflowStatus::Failed => hinted,
                _ => fallback,
            },
            OracleAction::Reply => match hinted {
                OracleWorkflowStatus::Complete | OracleWorkflowStatus::Failed => fallback,
                _ => hinted,
            },
            _ => match hinted {
                OracleWorkflowStatus::Complete | OracleWorkflowStatus::Failed => fallback,
                _ => hinted,
            },
        }
    }

    fn find_oracle_thread_by_orchestrator_thread(&self, thread_id: ThreadId) -> Option<ThreadId> {
        if self.oracle_state.orchestrator_thread_id == Some(thread_id) {
            return self.oracle_state.oracle_thread_id;
        }
        self.oracle_state
            .bindings
            .iter()
            .find_map(|(oracle_thread_id, binding)| {
                let cached = binding.orchestrator_thread_id == Some(thread_id);
                let workflow = binding
                    .workflow
                    .as_ref()
                    .is_some_and(|workflow| workflow.orchestrator_thread_id == Some(thread_id));
                (cached || workflow).then_some(*oracle_thread_id)
            })
    }

    fn notification_is_orchestrator_checkpoint_candidate(
        notification: &ServerNotification,
    ) -> bool {
        matches!(notification, ServerNotification::TurnCompleted(_))
    }

    fn notification_orchestrator_checkpoint_turn_id(
        notification: &ServerNotification,
    ) -> Option<&str> {
        match notification {
            ServerNotification::TurnCompleted(notification) => Some(notification.turn.id.as_str()),
            _ => None,
        }
    }

    fn thread_closed_preserves_orchestrator_checkpoint(
        &self,
        oracle_thread_id: ThreadId,
        thread_id: ThreadId,
    ) -> bool {
        if self
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .is_some_and(|binding| {
                binding.orchestrator_thread_id == Some(thread_id)
                    && (binding.phase == OracleSupervisorPhase::WaitingForOrchestrator
                        || (binding.phase
                            == OracleSupervisorPhase::WaitingForOracle(
                                OracleRequestKind::Checkpoint,
                            )
                            && binding.active_checkpoint_thread_id == Some(thread_id)))
            })
        {
            return true;
        }

        self.oracle_state.oracle_thread_id == Some(oracle_thread_id)
            && self.oracle_state.orchestrator_thread_id == Some(thread_id)
            && (self.oracle_state.phase == OracleSupervisorPhase::WaitingForOrchestrator
                || (self.oracle_state.phase
                    == OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint)
                    && self.oracle_state.active_checkpoint_thread_id == Some(thread_id)))
    }

    fn queue_orchestrator_checkpoint_candidate(
        &mut self,
        oracle_thread_id: ThreadId,
        thread_id: ThreadId,
        turn_id: Option<&str>,
    ) -> (bool, bool) {
        let workflow_version = self
            .oracle_state
            .workflow
            .as_ref()
            .map(|workflow| workflow.version);
        let mut already_pending = false;
        let mut is_head = false;
        if let Some(binding) = self.oracle_state.bindings.get_mut(&oracle_thread_id) {
            already_pending = binding.pending_checkpoint_threads.contains(&thread_id);
            if !already_pending {
                binding.pending_checkpoint_threads.push(thread_id);
            }
            if let Some(workflow_version) = workflow_version {
                binding
                    .pending_checkpoint_versions
                    .insert(thread_id, workflow_version);
            }
            if let Some(turn_id) = turn_id {
                binding
                    .pending_checkpoint_turn_ids
                    .insert(thread_id, turn_id.to_string());
            }
            is_head = binding.pending_checkpoint_threads.first().copied() == Some(thread_id);
        }
        (already_pending, is_head)
    }

    async fn handle_routed_orchestrator_notification(
        &mut self,
        thread_id: ThreadId,
        notification: &ServerNotification,
    ) {
        let Some(oracle_thread_id) = self.find_oracle_thread_by_orchestrator_thread(thread_id)
        else {
            return;
        };
        self.activate_oracle_binding(oracle_thread_id);

        if Self::notification_is_orchestrator_checkpoint_candidate(notification) {
            let (_, is_head) = self.queue_orchestrator_checkpoint_candidate(
                oracle_thread_id,
                thread_id,
                Self::notification_orchestrator_checkpoint_turn_id(notification),
            );
            self.persist_active_oracle_binding();
            if is_head
                && !matches!(
                    self.oracle_state.phase,
                    OracleSupervisorPhase::WaitingForOracle(_)
                )
            {
                self.emit_next_pending_oracle_checkpoint(oracle_thread_id);
            }
            return;
        }

        if !matches!(notification, ServerNotification::ThreadClosed(_)) {
            return;
        }

        if self.thread_closed_preserves_orchestrator_checkpoint(oracle_thread_id, thread_id) {
            self.mark_agent_picker_thread_closed(thread_id);
            self.thread_event_channels.remove(&thread_id);
            let (already_pending, is_head) =
                self.queue_orchestrator_checkpoint_candidate(oracle_thread_id, thread_id, None);
            self.persist_active_oracle_binding();
            if !already_pending
                && is_head
                && self.oracle_state.phase == OracleSupervisorPhase::WaitingForOrchestrator
            {
                self.emit_next_pending_oracle_checkpoint(oracle_thread_id);
            }
            return;
        }

        self.handle_routed_oracle_thread_closed(thread_id).await;
    }

    fn terminal_orchestrator_turn_id(thread: &codex_app_server_protocol::Thread) -> Option<&str> {
        thread
            .turns
            .last()
            .filter(|turn| {
                matches!(
                    turn.status,
                    TurnStatus::Completed | TurnStatus::Failed | TurnStatus::Interrupted
                )
            })
            .map(|turn| turn.id.as_str())
    }

    fn next_orchestrator_checkpoint_turn_id<'a>(
        binding: Option<&OracleThreadBinding>,
        thread_id: ThreadId,
        thread: &'a codex_app_server_protocol::Thread,
    ) -> Option<&'a str> {
        let latest_turn_id = Self::terminal_orchestrator_turn_id(thread)?;
        if binding
            .and_then(|binding| binding.last_checkpoint_turn_ids.get(&thread_id))
            .is_some_and(|turn_id| turn_id == latest_turn_id)
        {
            None
        } else {
            Some(latest_turn_id)
        }
    }

    fn orchestrator_checkpoint_retry_pending_turn<'a>(
        binding: Option<&'a OracleThreadBinding>,
        thread_id: ThreadId,
        thread: &'a codex_app_server_protocol::Thread,
    ) -> Option<(&'a str, &'a str)> {
        let pending_turn_id = binding
            .and_then(|binding| binding.pending_checkpoint_turn_ids.get(&thread_id))
            .map(String::as_str)?;
        let visible_terminal_turn_id = Self::terminal_orchestrator_turn_id(thread)?;
        (pending_turn_id != visible_terminal_turn_id)
            .then_some((pending_turn_id, visible_terminal_turn_id))
    }

    fn mark_active_oracle_checkpoint(
        &mut self,
        oracle_thread_id: ThreadId,
        thread_id: ThreadId,
        turn_id: String,
    ) {
        self.oracle_state.active_checkpoint_thread_id = Some(thread_id);
        self.oracle_state.active_checkpoint_turn_id = Some(turn_id.clone());
        if let Some(binding) = self.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.active_checkpoint_thread_id = Some(thread_id);
            binding.active_checkpoint_turn_id = Some(turn_id);
        }
    }

    fn clear_pending_oracle_checkpoint(&mut self, oracle_thread_id: ThreadId, thread_id: ThreadId) {
        if let Some(binding) = self.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding
                .pending_checkpoint_threads
                .retain(|pending| *pending != thread_id);
            binding.pending_checkpoint_versions.remove(&thread_id);
            binding.pending_checkpoint_turn_ids.remove(&thread_id);
        }
    }

    fn oracle_checkpoint_turn_is_known(
        &self,
        oracle_thread_id: ThreadId,
        thread_id: ThreadId,
    ) -> bool {
        self.oracle_state
            .bindings
            .get(&oracle_thread_id)
            .is_some_and(|binding| binding.pending_checkpoint_turn_ids.contains_key(&thread_id))
    }

    fn clear_closed_orchestrator_thread_binding(
        &mut self,
        oracle_thread_id: ThreadId,
        thread_id: ThreadId,
        blocker: &str,
    ) {
        if let Some(binding) = self.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding
                .pending_checkpoint_threads
                .retain(|pending| *pending != thread_id);
            binding.pending_checkpoint_versions.remove(&thread_id);
            binding.pending_checkpoint_turn_ids.remove(&thread_id);
            binding.last_checkpoint_turn_ids.remove(&thread_id);
            if binding.orchestrator_thread_id == Some(thread_id) {
                binding.orchestrator_thread_id = None;
            }
            if let Some(workflow) = binding.workflow.as_mut()
                && workflow.orchestrator_thread_id == Some(thread_id)
            {
                workflow.orchestrator_thread_id = None;
                workflow.status = OracleWorkflowStatus::Failed;
                workflow.last_blocker = Some(blocker.to_string());
            }
            Self::sync_legacy_oracle_binding_fields(binding);
        }
        if self.oracle_state.orchestrator_thread_id == Some(thread_id) {
            self.oracle_state.orchestrator_thread_id = None;
        }
        if let Some(workflow) = self.oracle_state.workflow.as_mut()
            && workflow.orchestrator_thread_id == Some(thread_id)
        {
            workflow.orchestrator_thread_id = None;
            workflow.status = OracleWorkflowStatus::Failed;
            workflow.last_blocker = Some(blocker.to_string());
        }
    }

    async fn fail_closed_orchestrator_checkpoint_without_turn(
        &mut self,
        oracle_thread_id: ThreadId,
        thread_id: ThreadId,
    ) {
        let blocker = "The orchestrator thread closed before reporting back.";
        let message = format!(
            "The orchestrator thread {thread_id} closed before reporting back. Oracle supervision is idle again."
        );
        self.clear_closed_orchestrator_thread_binding(oracle_thread_id, thread_id, blocker);
        if self.oracle_state.phase == OracleSupervisorPhase::WaitingForOrchestrator {
            self.oracle_state.phase = OracleSupervisorPhase::Idle;
            self.oracle_state.automatic_context_followups = 0;
        }
        self.oracle_state.last_status = Some(message.clone());
        self.append_oracle_status_event(oracle_thread_id, &message)
            .await;
        self.persist_active_oracle_binding();
    }

    fn emit_next_pending_oracle_checkpoint(&self, oracle_thread_id: ThreadId) {
        let next_thread = self
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .and_then(|binding| binding.pending_checkpoint_threads.first().copied());
        if let Some(thread_id) = next_thread {
            let workflow_version = self
                .oracle_state
                .bindings
                .get(&oracle_thread_id)
                .and_then(|binding| binding.pending_checkpoint_versions.get(&thread_id).copied());
            self.app_event_tx.send(AppEvent::OracleCheckpoint {
                thread_id,
                workflow_version,
            });
        }
    }

    fn schedule_orchestrator_checkpoint_retry(
        &self,
        thread_id: ThreadId,
        workflow_version: Option<u64>,
    ) {
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            app_event_tx.send(AppEvent::OracleCheckpoint {
                thread_id,
                workflow_version,
            });
        });
    }

    fn commit_active_oracle_checkpoint(&mut self, oracle_thread_id: ThreadId) {
        let thread_id = self.oracle_state.active_checkpoint_thread_id.take();
        let turn_id = self.oracle_state.active_checkpoint_turn_id.take();
        if let Some(binding) = self.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.active_checkpoint_thread_id = None;
            binding.active_checkpoint_turn_id = None;
            if let (Some(thread_id), Some(turn_id)) = (thread_id, turn_id) {
                binding.last_checkpoint_turn_ids.insert(thread_id, turn_id);
            }
        }
    }

    fn clear_oracle_checkpoint_state(&mut self, oracle_thread_id: ThreadId) {
        self.oracle_state.active_checkpoint_thread_id = None;
        self.oracle_state.active_checkpoint_turn_id = None;
        if let Some(binding) = self.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.pending_checkpoint_threads.clear();
            binding.pending_checkpoint_versions.clear();
            binding.pending_checkpoint_turn_ids.clear();
            binding.last_checkpoint_turn_ids.clear();
            binding.active_checkpoint_thread_id = None;
            binding.active_checkpoint_turn_id = None;
        }
    }

    fn clear_oracle_orchestrator_binding(&mut self, oracle_thread_id: ThreadId) {
        self.oracle_state.orchestrator_thread_id = None;
        self.clear_oracle_checkpoint_state(oracle_thread_id);
        if let Some(binding) = self.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.orchestrator_thread_id = None;
            if let Some(participant) = binding
                .participants
                .get_mut(ORACLE_ORCHESTRATOR_DESTINATION)
            {
                participant.route_completions = false;
                participant.route_closures = false;
            }
            Self::sync_legacy_oracle_binding_fields(binding);
        }
        if let Some(workflow) = self.oracle_state.workflow.as_mut() {
            workflow.orchestrator_thread_id = None;
        }
        self.refresh_routed_oracle_thread_owners(oracle_thread_id);
    }

    fn requeue_active_oracle_checkpoint(&mut self, oracle_thread_id: ThreadId) {
        let thread_id = self.oracle_state.active_checkpoint_thread_id.take();
        let turn_id = self.oracle_state.active_checkpoint_turn_id.take();
        let workflow_version = self
            .oracle_state
            .workflow
            .as_ref()
            .map(|workflow| workflow.version);
        if let Some(binding) = self.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.active_checkpoint_thread_id = None;
            binding.active_checkpoint_turn_id = None;
            if let Some(thread_id) = thread_id {
                if !binding.pending_checkpoint_threads.contains(&thread_id) {
                    binding.pending_checkpoint_threads.insert(0, thread_id);
                }
                if let Some(workflow_version) = workflow_version {
                    binding
                        .pending_checkpoint_versions
                        .insert(thread_id, workflow_version);
                }
                if let Some(turn_id) = turn_id {
                    binding
                        .pending_checkpoint_turn_ids
                        .entry(thread_id)
                        .or_insert(turn_id);
                }
            }
        }
    }

    fn requeue_orchestrator_checkpoint_launch_failure(
        &mut self,
        oracle_thread_id: ThreadId,
        thread_id: ThreadId,
        workflow_version: Option<u64>,
        turn_id: &str,
    ) {
        if let Some(binding) = self.oracle_state.bindings.get_mut(&oracle_thread_id) {
            if !binding.pending_checkpoint_threads.contains(&thread_id) {
                binding.pending_checkpoint_threads.insert(0, thread_id);
            }
            if let Some(workflow_version) = workflow_version {
                binding
                    .pending_checkpoint_versions
                    .insert(thread_id, workflow_version);
            }
            binding
                .pending_checkpoint_turn_ids
                .entry(thread_id)
                .or_insert_with(|| turn_id.to_string());
        }
        self.schedule_orchestrator_checkpoint_retry(thread_id, workflow_version);
    }

    fn defer_active_oracle_checkpoint(&mut self, oracle_thread_id: ThreadId) -> bool {
        let thread_id = self.oracle_state.active_checkpoint_thread_id.take();
        let turn_id = self.oracle_state.active_checkpoint_turn_id.take();
        let workflow_version = self
            .oracle_state
            .workflow
            .as_ref()
            .map(|workflow| workflow.version);
        let mut had_other_pending = false;
        if let Some(binding) = self.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.active_checkpoint_thread_id = None;
            binding.active_checkpoint_turn_id = None;
            if let Some(thread_id) = thread_id {
                had_other_pending = binding
                    .pending_checkpoint_threads
                    .iter()
                    .any(|pending| *pending != thread_id)
                    || binding
                        .pending_checkpoint_turn_ids
                        .get(&thread_id)
                        .is_some_and(|pending_turn_id| {
                            Some(pending_turn_id.as_str()) != turn_id.as_deref()
                        });
                if !binding.pending_checkpoint_threads.contains(&thread_id) {
                    binding.pending_checkpoint_threads.push(thread_id);
                }
                if let Some(workflow_version) = workflow_version {
                    binding
                        .pending_checkpoint_versions
                        .insert(thread_id, workflow_version);
                }
                if let Some(turn_id) = turn_id {
                    binding
                        .pending_checkpoint_turn_ids
                        .entry(thread_id)
                        .or_insert(turn_id);
                }
            }
        }
        if !had_other_pending && let Some(thread_id) = thread_id {
            self.schedule_orchestrator_checkpoint_retry(thread_id, workflow_version);
        }
        had_other_pending
    }

    fn sync_legacy_oracle_binding_fields(binding: &mut OracleThreadBinding) {
        if let Some(workflow) = binding.workflow.as_mut() {
            workflow.orchestrator_thread_id = binding.orchestrator_thread_id;
            if workflow.summary.is_none() {
                workflow.summary = binding.last_orchestrator_task.clone();
            }
            return;
        }
        if binding.orchestrator_thread_id.is_none() {
            binding.last_orchestrator_task = None;
        }
    }

    fn refresh_routed_oracle_thread_owners(&mut self, oracle_thread_id: ThreadId) {
        self.oracle_state
            .routed_thread_owner
            .retain(|_, owner| owner.oracle_thread_id != oracle_thread_id);
        let Some(binding) = self.oracle_state.bindings.get(&oracle_thread_id) else {
            return;
        };
        for participant in binding.participants.values() {
            let Some(thread_id) = participant.thread_id else {
                continue;
            };
            if participant.address == ORACLE_HUMAN_DESTINATION {
                continue;
            }
            if !participant.route_completions && !participant.route_closures {
                continue;
            }
            self.oracle_state.routed_thread_owner.insert(
                thread_id,
                OracleRoutedThreadOwner {
                    oracle_thread_id,
                    address: participant.address.clone(),
                },
            );
        }
    }

    fn oracle_thread_ids_by_conversation_id(&self, conversation_id: &str) -> Vec<ThreadId> {
        let mut matches = self
            .oracle_state
            .bindings
            .iter()
            .filter_map(|(thread_id, binding)| {
                (binding.conversation_id.as_deref() == Some(conversation_id)).then_some(*thread_id)
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(std::string::ToString::to_string);
        matches
    }

    fn find_unique_oracle_thread_by_conversation_id(
        &self,
        conversation_id: &str,
    ) -> Result<Option<ThreadId>> {
        let matches = self.oracle_thread_ids_by_conversation_id(conversation_id);
        match matches.as_slice() {
            [] => Ok(None),
            [thread_id] => Ok(Some(*thread_id)),
            [first, second, ..] => Err(color_eyre::eyre::eyre!(
                "Oracle conversation {conversation_id} is already bound to multiple local threads ({first}, {second}); refusing to guess which local binding owns it."
            )),
        }
    }

    fn oracle_thread_label(&self, thread_id: ThreadId) -> String {
        self.oracle_state
            .bindings
            .get(&thread_id)
            .and_then(|binding| binding.remote_title.clone())
            .filter(|title| !title.trim().is_empty())
            .map(|title| format!("Oracle: {title}"))
            .unwrap_or_else(|| "Oracle".to_string())
    }

    fn oracle_followup_session_anchor_thread_id(&self) -> Option<ThreadId> {
        self.current_displayed_thread_id()
            .filter(|thread_id| self.is_visible_oracle_thread(*thread_id))
            .or(self.oracle_state.oracle_thread_id)
    }

    fn preferred_oracle_followup_session_for_anchor(
        &self,
        anchor_thread_id: Option<ThreadId>,
    ) -> Option<String> {
        anchor_thread_id
            .and_then(|thread_id| self.oracle_control_followup_session_for_thread(thread_id))
    }

    fn preferred_oracle_followup_session(&self) -> Option<String> {
        self.preferred_oracle_followup_session_for_anchor(
            self.oracle_followup_session_anchor_thread_id(),
        )
    }

    fn require_oracle_followup_session_for_anchor(
        &self,
        anchor_thread_id: Option<ThreadId>,
    ) -> Result<String> {
        self.preferred_oracle_followup_session_for_anchor(anchor_thread_id)
            .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "Oracle needs an attached local browser session before remote thread controls are available."
            )
        })
    }

    fn oracle_session_matches_root_slug(session_id: &str, session_root_slug: &str) -> bool {
        session_id == session_root_slug
            || session_id
                .strip_prefix(session_root_slug)
                .is_some_and(|suffix| suffix.starts_with('-'))
    }

    fn oracle_conversation_target(url: &str) -> Option<(String, String)> {
        let parsed = Url::parse(url).ok()?;
        let segments = parsed
            .path_segments()?
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        match segments.as_slice() {
            ["c", conversation_id] if !conversation_id.is_empty() => {
                Some((String::new(), (*conversation_id).to_string()))
            }
            ["g", project_id, "c", conversation_id]
                if !project_id.is_empty() && !conversation_id.is_empty() =>
            {
                Some((format!("g/{project_id}"), (*conversation_id).to_string()))
            }
            _ => None,
        }
    }

    fn requested_oracle_conversation_url_matches_remote(
        conversation_id: &str,
        requested_url: Option<&str>,
        remote_url: Option<&str>,
    ) -> bool {
        let Some(requested_url) = requested_url else {
            return true;
        };
        let Some((requested_scope, requested_conversation_id)) =
            Self::oracle_conversation_target(requested_url)
        else {
            return false;
        };
        let Some((remote_scope, remote_conversation_id)) =
            remote_url.and_then(Self::oracle_conversation_target)
        else {
            return false;
        };
        requested_conversation_id == conversation_id
            && remote_conversation_id == conversation_id
            && requested_scope == remote_scope
    }

    fn oracle_control_followup_session_for_thread(&self, thread_id: ThreadId) -> Option<String> {
        let binding = self.oracle_state.bindings.get(&thread_id)?;
        let session_id = binding.current_session_id.clone()?;
        match binding.current_session_ownership.as_ref() {
            Some(OracleSessionOwnership::BrokerThread) if binding.current_session_verified => {
                Some(session_id)
            }
            Some(OracleSessionOwnership::SessionRootSlug(root_slug))
                if binding.session_root_slug.as_deref() == Some(root_slug.as_str())
                    && Self::oracle_session_matches_root_slug(session_id.as_str(), root_slug) =>
            {
                Some(session_id)
            }
            None if binding.session_root_slug.as_ref().is_some_and(|root_slug| {
                Self::oracle_session_matches_root_slug(session_id.as_str(), root_slug)
            }) =>
            {
                Some(session_id)
            }
            _ => None,
        }
    }

    fn oracle_followup_session_for_thread(&self, thread_id: ThreadId) -> Option<String> {
        let binding = self.oracle_state.bindings.get(&thread_id)?;
        let session_id = binding.current_session_id.clone()?;
        match binding.current_session_ownership.as_ref() {
            Some(OracleSessionOwnership::BrokerThread) => Some(session_id),
            Some(OracleSessionOwnership::SessionRootSlug(root_slug))
                if binding.session_root_slug.as_deref() == Some(root_slug.as_str())
                    && Self::oracle_session_matches_root_slug(session_id.as_str(), root_slug) =>
            {
                Some(session_id)
            }
            None if binding.session_root_slug.as_ref().is_some_and(|root_slug| {
                Self::oracle_session_matches_root_slug(session_id.as_str(), root_slug)
            }) =>
            {
                Some(session_id)
            }
            _ => None,
        }
    }

    fn oracle_broker_thread_session_requires_rebind(&self, thread_id: ThreadId) -> bool {
        self.oracle_state
            .bindings
            .get(&thread_id)
            .is_some_and(|binding| {
                matches!(
                    binding.current_session_ownership,
                    Some(OracleSessionOwnership::BrokerThread)
                ) && binding.current_session_id.is_some()
                    && binding.conversation_id.is_some()
                    && !binding.current_session_verified
            })
    }

    fn ensure_oracle_session_slug_for_thread(&mut self, thread_id: ThreadId) -> String {
        if let Some(slug) = self.oracle_state.session_root_slug.clone() {
            return slug;
        }
        let slug = generate_session_slug(thread_id);
        self.oracle_state.session_root_slug = Some(slug.clone());
        if let Some(binding) = self.oracle_state.bindings.get_mut(&thread_id) {
            binding.session_root_slug = Some(slug.clone());
        }
        slug
    }

    async fn ensure_oracle_followup_session_for_run(
        &mut self,
        thread_id: ThreadId,
    ) -> Result<Option<String>> {
        if let Some(session_id) = self.oracle_followup_session_for_thread(thread_id)
            && !self.oracle_broker_thread_session_requires_rebind(thread_id)
        {
            return Ok(Some(session_id));
        }
        let Some(binding) = self.oracle_state.bindings.get(&thread_id).cloned() else {
            return Ok(None);
        };
        let Some(conversation_id) = binding.conversation_id.clone() else {
            return Ok(None);
        };
        let mut conflicting_thread_ids = self
            .oracle_state
            .bindings
            .iter()
            .filter_map(|(bound_thread_id, bound_binding)| {
                (*bound_thread_id != thread_id
                    && bound_binding.conversation_id.as_deref() == Some(conversation_id.as_str()))
                .then_some(*bound_thread_id)
            })
            .collect::<Vec<_>>();
        conflicting_thread_ids.sort_by_key(std::string::ToString::to_string);
        if let Some(conflicting_thread_id) = conflicting_thread_ids.into_iter().next() {
            return Err(color_eyre::eyre::eyre!(
                "Oracle conversation {conversation_id} is already bound to local thread {conflicting_thread_id}; refusing to reattach it onto thread {thread_id}."
            ));
        }
        // After an interrupt we may lose the reusable follow-up session while
        // still knowing which remote Oracle thread this local binding owns.
        // Reattach that thread before starting a new run instead of silently
        // drifting into a fresh ChatGPT conversation.
        let rebound = self
            .attach_oracle_thread_binding(
                conversation_id.clone(),
                None,
                binding.remote_title.clone(),
            )
            .await?;
        let rebound_thread_id = rebound.thread_id();
        if rebound_thread_id != thread_id {
            return Err(color_eyre::eyre::eyre!(
                "Oracle reattached conversation {conversation_id} onto thread {rebound_thread_id} instead of requested thread {thread_id}; refusing to steal another local Oracle binding."
            ));
        }
        let Some(session_id) = self.oracle_followup_session_for_thread(rebound_thread_id) else {
            return Err(color_eyre::eyre::eyre!(
                "Oracle reattached conversation {conversation_id} but did not return a reusable hidden browser session."
            ));
        };
        Ok(Some(session_id))
    }

    async fn ensure_oracle_session_continuity_for_run(
        &mut self,
        thread_id: ThreadId,
    ) -> Result<(String, Option<String>)> {
        let session_slug = self.ensure_oracle_session_slug_for_thread(thread_id);
        let followup_session = self
            .ensure_oracle_followup_session_for_run(thread_id)
            .await?;
        Ok((session_slug, followup_session))
    }

    fn oracle_browser_is_busy(&self) -> bool {
        self.oracle_state.bindings.values().any(|binding| {
            matches!(
                binding.phase,
                OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn)
                    | OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint)
            )
        })
    }

    fn flatten_user_turn_text(items: &[UserInput]) -> String {
        let mut parts = Vec::new();
        for item in items {
            match item {
                UserInput::Text { text, .. } => parts.push(text.clone()),
                UserInput::Image { image_url } => parts.push(format!(
                    "[image: {}]",
                    Self::sanitize_user_input_marker(image_url)
                )),
                UserInput::LocalImage { path } => parts.push(format!(
                    "[local_image: {}]",
                    Self::sanitize_user_input_marker(&path.display().to_string())
                )),
                UserInput::Mention { name, path } => parts.push(format!(
                    "[mention: {} -> {}]",
                    Self::sanitize_user_input_marker(name),
                    Self::sanitize_user_input_marker(path)
                )),
                UserInput::Skill { name, path } => parts.push(format!(
                    "[skill: {} -> {}]",
                    Self::sanitize_user_input_marker(name),
                    Self::sanitize_user_input_marker(&path.display().to_string())
                )),
                _ => {}
            }
        }
        parts.join("\n")
    }

    fn flatten_user_turn_source_text(items: &[UserInput]) -> String {
        items
            .iter()
            .filter_map(|item| match item {
                UserInput::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sanitize_user_input_marker(value: &str) -> String {
        value
            .chars()
            .map(|ch| if matches!(ch, '\r' | '\n') { ' ' } else { ch })
            .collect()
    }

    fn oracle_unsupported_user_inputs(items: &[UserInput]) -> Vec<&'static str> {
        let mut unsupported = items
            .iter()
            .filter_map(|item| match item {
                UserInput::Image { .. } => Some("remote images"),
                UserInput::Mention { .. } => Some("mentions"),
                UserInput::Skill { .. } => Some("skills"),
                _ => None,
            })
            .collect::<Vec<_>>();
        unsupported.sort_unstable();
        unsupported.dedup();
        unsupported
    }

    fn oracle_run_missing_required_control(result: &OracleRunResult) -> bool {
        result.requires_control
            && (result.response.directive.is_none() || result.response.control_issue.is_some())
    }

    fn oracle_control_repair_exhausted(result: &OracleRunResult) -> bool {
        Self::oracle_run_missing_required_control(result) && result.repair_attempt > 0
    }

    fn oracle_control_repair_message(&self, result: &OracleRunResult) -> Option<String> {
        if let Some(issue) = result.response.control_issue.as_ref() {
            return Some(issue.message.clone());
        }
        let Some(directive) = result.response.directive.as_ref() else {
            return Self::oracle_run_missing_required_control(result).then_some(
                "Oracle reply was missing the required final `oracle_control` block.".to_string(),
            );
        };
        if directive.schema_version != Some(ORACLE_CONTROL_SCHEMA_VERSION) {
            return Some(format!(
                "Oracle control block must include `schema_version: {ORACLE_CONTROL_SCHEMA_VERSION}`."
            ));
        }
        if directive.op_id.as_deref().is_none() {
            return Some("Oracle control block is missing `op_id`.".to_string());
        }
        if directive.idempotency_key.as_deref().is_none() {
            return Some("Oracle control block is missing `idempotency_key`.".to_string());
        }
        if directive.workflow_version == Some(0) {
            return Some(
                "Oracle control block must include a positive `workflow_version` starting at 1."
                    .to_string(),
            );
        }
        if let Some(workflow) = self.oracle_state.workflow.as_ref().filter(|workflow| {
            !matches!(
                workflow.status,
                OracleWorkflowStatus::Complete | OracleWorkflowStatus::Failed
            )
        }) {
            if directive.workflow_id.as_deref().is_none() {
                return Some(format!(
                    "Oracle control block must include the active `workflow_id` (`{}`).",
                    workflow.workflow_id
                ));
            }
            if directive.workflow_version.is_none() {
                return Some(format!(
                    "Oracle control block must include the active `workflow_version` ({}) for workflow `{}`.",
                    workflow.version, workflow.workflow_id
                ));
            }
        }
        if self.oracle_state.workflow.is_none()
            && matches!(result.response.action, OracleAction::Delegate)
        {
            if directive.workflow_id.as_deref().is_none() {
                return Some(
                    "Oracle control block must include an explicit `workflow_id` when starting the first workflow handoff."
                        .to_string(),
                );
            }
            if directive.workflow_version.is_none() {
                return Some(
                    "Oracle control block must include an explicit `workflow_version` when starting the first workflow handoff."
                        .to_string(),
                );
            }
        }
        if self.oracle_state.accept_user_turn_workflow_replacement
            && self.oracle_state.workflow.as_ref().is_some_and(|workflow| {
                matches!(
                    workflow.status,
                    OracleWorkflowStatus::Complete | OracleWorkflowStatus::Failed
                )
            })
            && !matches!(result.response.action, OracleAction::Reply)
        {
            if directive.workflow_id.as_deref().is_none() {
                return Some(
                    "Oracle control block must include an explicit new `workflow_id` when replacing a completed workflow."
                        .to_string(),
                );
            }
            if directive.workflow_version.is_none() {
                return Some(
                    "Oracle control block must include an explicit new `workflow_version` when replacing a completed workflow."
                        .to_string(),
                );
            }
        }
        None
    }

    fn oracle_duplicate_control_message(
        &self,
        directive: &OracleControlDirective,
    ) -> Option<String> {
        let workflow = self.oracle_state.workflow.as_ref()?;
        if self.oracle_state.accept_user_turn_workflow_replacement
            && matches!(
                workflow.status,
                OracleWorkflowStatus::Complete | OracleWorkflowStatus::Failed
            )
            && directive
                .workflow_id
                .as_deref()
                .is_some_and(|workflow_id| workflow_id != workflow.workflow_id)
        {
            return None;
        }
        if let Some(op_id) = directive.op_id.as_deref()
            && (workflow.pending_op_ids.contains(op_id) || workflow.applied_op_ids.contains(op_id))
        {
            return Some(format!(
                "Ignored duplicate Oracle control op `{op_id}` for workflow `{}`.",
                workflow.workflow_id
            ));
        }
        if let Some(idempotency_key) = directive.idempotency_key.as_deref()
            && (workflow.pending_idempotency_keys.contains(idempotency_key)
                || workflow.applied_idempotency_keys.contains(idempotency_key))
        {
            return Some(format!(
                "Ignored duplicate Oracle control retry `{idempotency_key}` for workflow `{}`.",
                workflow.workflow_id
            ));
        }
        None
    }

    fn reserve_pending_oracle_control(&mut self, directive: &OracleControlDirective) {
        let Some(workflow) = self.oracle_state.workflow.as_mut() else {
            return;
        };
        if let Some(op_id) = directive.op_id.as_ref() {
            workflow.pending_op_ids.insert(op_id.clone());
        }
        if let Some(idempotency_key) = directive.idempotency_key.as_ref() {
            workflow
                .pending_idempotency_keys
                .insert(idempotency_key.clone());
        }
    }

    fn release_pending_oracle_control(&mut self, directive: &OracleControlDirective) {
        let Some(workflow) = self.oracle_state.workflow.as_mut() else {
            return;
        };
        if let Some(op_id) = directive.op_id.as_ref() {
            workflow.pending_op_ids.remove(op_id);
        }
        if let Some(idempotency_key) = directive.idempotency_key.as_ref() {
            workflow.pending_idempotency_keys.remove(idempotency_key);
        }
    }

    fn record_applied_oracle_control(&mut self, directive: &OracleControlDirective) {
        let Some(workflow) = self.oracle_state.workflow.as_mut() else {
            return;
        };
        if let Some(op_id) = directive.op_id.as_ref() {
            workflow.pending_op_ids.remove(op_id);
            workflow.applied_op_ids.insert(op_id.clone());
        }
        if let Some(idempotency_key) = directive.idempotency_key.as_ref() {
            workflow.pending_idempotency_keys.remove(idempotency_key);
            workflow
                .applied_idempotency_keys
                .insert(idempotency_key.clone());
        }
    }

    fn natural_language_oracle_invocation(items: &[UserInput]) -> bool {
        let text = Self::flatten_user_turn_text(items);
        let Some(first_line) = text.lines().map(str::trim).find(|line| !line.is_empty()) else {
            return false;
        };
        if first_line.starts_with(['"', '\'', '`', '>']) {
            return false;
        }
        let normalized = first_line.to_ascii_lowercase();
        let normalized = normalized
            .strip_prefix("please ")
            .or_else(|| normalized.strip_prefix("can you "))
            .or_else(|| normalized.strip_prefix("could you "))
            .or_else(|| normalized.strip_prefix("would you "))
            .unwrap_or(normalized.as_str());
        [
            "use your oracle skill to",
            "use the oracle skill to",
            "use oracle skill to",
            "use your oracle to",
            "use oracle to",
        ]
        .iter()
        .any(|needle| normalized.starts_with(needle))
    }

    fn start_oracle_run(&mut self, request: OracleRunRequest) -> Result<()> {
        self.activate_oracle_binding(request.oracle_thread_id);
        let broker = self.ensure_oracle_broker(request.oracle_repo.as_path())?;
        let run_id = format!("oracle-run-{}", Uuid::new_v4());
        self.oracle_state.phase = OracleSupervisorPhase::WaitingForOracle(request.kind);
        self.oracle_state.active_run_id = Some(run_id.clone());
        self.oracle_state.aborted_run_id = None;
        self.oracle_state
            .inflight_runs
            .insert(request.oracle_thread_id, run_id.clone());
        self.oracle_state
            .inflight_run_requests
            .insert(run_id.clone(), request.clone());
        self.oracle_state.aborted_runs.remove(&run_id);
        self.persist_active_oracle_binding();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let oracle_thread_id = request.oracle_thread_id;
            let kind = request.kind;
            let session_slug = request.session_slug.clone();
            let requested_prompt = request.requested_prompt.clone();
            let source_user_text = request.source_user_text.clone();
            let files = request.files.clone();
            let requires_control = request.requires_control;
            let repair_attempt = request.repair_attempt;
            let broker_result = broker
                .request(OracleBrokerRequest {
                    prompt: request.prompt.clone(),
                    session_slug: request.session_slug.clone(),
                    followup_session: request.followup_session.clone(),
                    files: request.files.clone(),
                    cwd: request.workspace_cwd.clone(),
                    model: request.model.model_id().to_string(),
                    browser_model_strategy: request.browser_model_strategy.clone(),
                    browser_model_label: request.browser_model_label.clone(),
                    browser_thinking_time: request.browser_thinking_time.clone(),
                })
                .await;
            let result = broker_result.map(|response| {
                remember_oracle_run_remote_metadata(
                    run_id.as_str(),
                    response.conversation_id,
                    response.title,
                );
                OracleRunResult {
                    run_id: run_id.clone(),
                    oracle_thread_id,
                    kind,
                    requested_slug: session_slug.clone(),
                    session_id: response.session_id.unwrap_or(session_slug.clone()),
                    requested_prompt,
                    source_user_text,
                    files,
                    requires_control,
                    repair_attempt,
                    response: parse_oracle_response(response.output.as_deref().unwrap_or("")),
                }
            });
            match result {
                Ok(result) => tx.send(AppEvent::OracleRunCompleted { result }),
                Err(error) => tx.send(AppEvent::OracleRunFailed {
                    run_id,
                    visible_thread_id: oracle_thread_id,
                    kind,
                    session_slug,
                    error,
                }),
            }
        });
        Ok(())
    }

    fn shutdown_oracle_broker(&mut self, abort_inflight: bool) {
        if let Some((_repo, broker)) = self.oracle_broker.take() {
            if abort_inflight {
                broker.abort();
            } else {
                tokio::spawn(async move {
                    let _ = broker.shutdown().await;
                });
            }
        }
    }

    async fn shutdown_oracle_broker_gracefully(&mut self) {
        if let Some((_repo, broker)) = self.oracle_broker.take()
            && let Err(err) = broker.shutdown().await
        {
            tracing::warn!(%err, "failed to gracefully shut down Oracle broker");
        }
    }

    async fn shutdown_oracle_broker_await(&mut self, abort_inflight: bool) {
        if let Some((_repo, broker)) = self.oracle_broker.take() {
            if abort_inflight {
                broker.abort_and_wait().await;
            } else if let Err(err) = broker.shutdown().await {
                tracing::warn!(%err, "failed to gracefully shut down Oracle broker");
            }
        }
    }

    fn oracle_interrupt_message(kind: OracleRequestKind) -> &'static str {
        match kind {
            OracleRequestKind::UserTurn => {
                "Oracle request interrupted. The browser run was aborted."
            }
            OracleRequestKind::Checkpoint => {
                "Oracle checkpoint review interrupted. The browser run was aborted."
            }
        }
    }

    fn consume_aborted_oracle_run(&mut self, run_id: &str) -> bool {
        let mut consumed = self.oracle_state.aborted_runs.remove(run_id);
        if self.oracle_state.aborted_run_id.as_deref() == Some(run_id) {
            self.oracle_state.aborted_run_id = None;
            consumed = true;
        }
        if consumed {
            self.oracle_state.inflight_run_requests.remove(run_id);
            self.oracle_state
                .inflight_runs
                .retain(|_, active_run_id| active_run_id != run_id);
        }
        consumed
    }

    fn is_active_oracle_run(&self, thread_id: ThreadId, run_id: &str) -> bool {
        self.oracle_state
            .inflight_runs
            .get(&thread_id)
            .is_some_and(|active| active == run_id)
            || (self.oracle_state.oracle_thread_id == Some(thread_id)
                && self.oracle_state.active_run_id.as_deref() == Some(run_id))
    }

    fn clear_oracle_run_tracking(&mut self) {
        self.oracle_state.active_run_id = None;
        self.oracle_state.aborted_run_id = None;
    }

    fn set_oracle_thread_session_state(
        &mut self,
        thread_id: ThreadId,
        session_id: Option<String>,
        ownership: Option<OracleSessionOwnership>,
    ) {
        let binding = self
            .oracle_state
            .bindings
            .entry(thread_id)
            .or_insert_with(|| OracleThreadBinding {
                phase: OracleSupervisorPhase::Idle,
                ..Default::default()
            });
        binding.current_session_id = session_id.clone();
        binding.current_session_ownership = ownership.clone();
        if session_id.is_none() || !matches!(ownership, Some(OracleSessionOwnership::BrokerThread))
        {
            binding.current_session_verified = false;
        }
        if self.oracle_state.oracle_thread_id == Some(thread_id) {
            self.oracle_state.current_session_id = session_id;
            self.oracle_state.current_session_ownership = ownership;
        }
    }

    fn clear_oracle_thread_session_root_slug(&mut self, thread_id: ThreadId) {
        if let Some(binding) = self.oracle_state.bindings.get_mut(&thread_id) {
            binding.session_root_slug = None;
        }
        if self.oracle_state.oracle_thread_id == Some(thread_id) {
            self.oracle_state.session_root_slug = None;
        }
    }

    fn set_oracle_thread_broker_session(
        &mut self,
        thread_id: ThreadId,
        session_id: Option<String>,
        verified: bool,
    ) {
        let ownership = session_id
            .as_ref()
            .map(|_| OracleSessionOwnership::BrokerThread);
        self.set_oracle_thread_session_state(thread_id, session_id.clone(), ownership);
        if let Some(binding) = self.oracle_state.bindings.get_mut(&thread_id) {
            binding.current_session_verified = verified && session_id.is_some();
        }
        self.clear_oracle_thread_session_root_slug(thread_id);
    }

    fn set_oracle_thread_broker_session_id(
        &mut self,
        thread_id: ThreadId,
        session_id: Option<String>,
    ) {
        self.set_oracle_thread_broker_session(thread_id, session_id, false);
    }

    fn set_oracle_thread_run_session_id(
        &mut self,
        thread_id: ThreadId,
        session_id: Option<String>,
        session_root_slug: String,
    ) {
        let ownership = session_id
            .as_ref()
            .map(|_| OracleSessionOwnership::SessionRootSlug(session_root_slug.clone()));
        self.set_oracle_thread_session_state(thread_id, session_id, ownership);
        if let Some(binding) = self.oracle_state.bindings.get_mut(&thread_id) {
            binding.session_root_slug = Some(session_root_slug);
        }
    }

    fn oracle_repo_not_found_message() -> &'static str {
        "Unable to locate forks/oracle or external/oracle from the current workspace."
    }

    fn oracle_abort_message(phase: OracleSupervisorPhase) -> Option<&'static str> {
        match phase {
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn) => Some(
                "Oracle mode was disabled while an Oracle turn was in progress. The browser run was aborted.",
            ),
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint) => Some(
                "Oracle mode was disabled while Oracle was reviewing an orchestrator checkpoint. The browser run was aborted.",
            ),
            OracleSupervisorPhase::WaitingForOrchestrator => Some(
                "Oracle mode was disabled while the orchestrator was still working. The supervision workflow was aborted.",
            ),
            OracleSupervisorPhase::Disabled | OracleSupervisorPhase::Idle => None,
        }
    }

    fn oracle_status_thread_event(message: &str) -> OracleWorkflowThreadEvent {
        OracleWorkflowThreadEvent {
            title: message.trim().to_string(),
            details: Vec::new(),
        }
    }

    fn oracle_delegate_thread_event(
        workflow: &OracleWorkflowBinding,
        message_for_user: &str,
    ) -> OracleWorkflowThreadEvent {
        let mut details = vec![format!(
            "Workflow: {} v{}",
            workflow.workflow_id, workflow.version
        )];
        let note = message_for_user.trim();
        if !note.is_empty()
            && !note.eq_ignore_ascii_case("Oracle delegated work to the orchestrator.")
        {
            details.push(format!("Note: {note}"));
        }
        OracleWorkflowThreadEvent {
            title: "Oracle delegated work to the orchestrator.".to_string(),
            details,
        }
    }

    fn oracle_context_request_thread_event(
        requested_context: &[String],
        message_for_user: &str,
    ) -> OracleWorkflowThreadEvent {
        let mut details = Vec::new();
        if !requested_context.is_empty() {
            details.push(format!("Requested: {}", requested_context.join(", ")));
        }
        let note = message_for_user.trim();
        if !note.is_empty()
            && !note
                .to_ascii_lowercase()
                .starts_with("oracle requested more context")
        {
            details.push(format!("Note: {note}"));
        }
        OracleWorkflowThreadEvent {
            title: "Oracle requested more context.".to_string(),
            details,
        }
    }

    fn oracle_delegate_user_message(message_for_user: &str) -> String {
        let objective = "Oracle delegated work to the orchestrator.";
        let note = message_for_user.trim();
        if note.is_empty() || note == objective {
            objective.to_string()
        } else {
            format!("{objective}\n\nOracle note:\n{note}")
        }
    }

    fn cached_oracle_broker_matches(
        cached_repo: &Path,
        oracle_repo: &Path,
        broker: &OracleBrokerClient,
    ) -> bool {
        cached_repo == oracle_repo && broker.is_usable()
    }

    fn ensure_oracle_broker(&mut self, oracle_repo: &Path) -> Result<OracleBrokerClient> {
        if let Some((repo, broker)) = &self.oracle_broker
            && Self::cached_oracle_broker_matches(repo.as_path(), oracle_repo, broker)
        {
            return Ok(broker.clone());
        }
        self.shutdown_oracle_broker(false);
        let broker =
            spawn_oracle_broker(oracle_repo).map_err(|err| color_eyre::eyre::eyre!(err))?;
        self.oracle_broker = Some((oracle_repo.to_path_buf(), broker.clone()));
        Ok(broker)
    }

    async fn disable_oracle_mode(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
    ) -> Result<()> {
        self.persist_active_oracle_binding();
        let bindings = self
            .oracle_state
            .bindings
            .iter()
            .map(|(thread_id, binding)| (*thread_id, binding.clone()))
            .collect::<Vec<_>>();
        if let Some(active_thread_id) = self.active_thread_id
            && self.is_visible_oracle_thread(active_thread_id)
            && let Some(primary_thread_id) = self.primary_thread_id
            && primary_thread_id != active_thread_id
        {
            self.select_agent_thread(tui, app_server, primary_thread_id)
                .await?;
        }
        let abort_inflight_oracle_run = bindings.iter().any(|(_, binding)| {
            matches!(
                binding.phase,
                OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn)
                    | OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint)
            )
        });
        for (oracle_thread_id, binding) in &bindings {
            let mut abort_message = Self::oracle_abort_message(binding.phase).map(str::to_string);
            if binding.phase == OracleSupervisorPhase::WaitingForOrchestrator
                && let Some(orchestrator_thread_id) = binding.orchestrator_thread_id
            {
                abort_message = Some(
                    "Oracle mode was disabled while the orchestrator was still working. The supervision workflow was aborted."
                        .to_string(),
                );
                if let Some(turn_id) = self.active_turn_id_for_thread(orchestrator_thread_id).await
                {
                    abort_message =
                        match app_server.turn_interrupt(orchestrator_thread_id, turn_id).await {
                            Ok(()) => Some(
                                "Oracle mode was disabled while the orchestrator was still working. The active orchestrator turn was interrupted and the supervision workflow was aborted."
                                    .to_string(),
                            ),
                            Err(err) => {
                                tracing::warn!(
                                    %orchestrator_thread_id,
                                    %err,
                                    "failed to interrupt oracle orchestrator turn during shutdown"
                                );
                                Some(
                                    "Oracle mode was disabled while the orchestrator was still working. Codex could not interrupt that turn cleanly, so background work may still be winding down."
                                        .to_string(),
                                )
                            }
                        };
                }
            }
            if let Some(message) = abort_message.as_deref() {
                self.activate_oracle_binding(*oracle_thread_id);
                self.oracle_state.last_status = Some(message.to_string());
                if self.oracle_state.pending_turn_id.is_some() {
                    self.complete_pending_oracle_turn(*oracle_thread_id, message)
                        .await;
                } else {
                    self.append_oracle_agent_turn(*oracle_thread_id, message)
                        .await;
                }
            }
            self.mark_agent_picker_thread_closed(*oracle_thread_id);
            if let Some(orchestrator_thread_id) = binding.orchestrator_thread_id {
                if let Err(err) = app_server.thread_unsubscribe(orchestrator_thread_id).await {
                    tracing::warn!(
                        %orchestrator_thread_id,
                        %err,
                        "failed to unsubscribe oracle orchestrator thread during shutdown"
                    );
                }
                self.thread_event_channels.remove(&orchestrator_thread_id);
            }
        }
        let model = self.oracle_state.model;
        self.shutdown_oracle_broker_await(abort_inflight_oracle_run)
            .await;
        self.oracle_state = OracleSupervisorState {
            model,
            phase: OracleSupervisorPhase::Disabled,
            last_status: Some("Oracle mode disabled.".to_string()),
            ..Default::default()
        };
        self.refresh_pending_thread_approvals().await;
        Ok(())
    }

    async fn interrupt_oracle_thread(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
    ) -> Result<bool> {
        if !self.is_visible_oracle_thread(thread_id) {
            return Ok(false);
        }
        self.activate_oracle_binding(thread_id);
        match self.oracle_state.phase {
            OracleSupervisorPhase::WaitingForOracle(kind) => {
                let message = Self::oracle_interrupt_message(kind).to_string();
                self.oracle_state.phase = OracleSupervisorPhase::Idle;
                let aborted_run_id = self.oracle_state.active_run_id.take();
                self.oracle_state.aborted_run_id = aborted_run_id.clone();
                self.oracle_state.inflight_runs.remove(&thread_id);
                if let Some(run_id) = aborted_run_id {
                    self.oracle_state.inflight_run_requests.remove(&run_id);
                    crate::session_log::log_oracle_run_interrupted(thread_id, &run_id, kind);
                    self.oracle_state.aborted_runs.insert(run_id);
                }
                // An interrupted browser run can leave the hidden follow-up
                // session unusable. Drop the local session chain so the next
                // Oracle turn reattaches cleanly, but preserve the remote
                // conversation binding so restart/resume stays on the same
                // ChatGPT thread.
                self.set_oracle_thread_session_state(thread_id, None, None);
                self.clear_oracle_thread_session_root_slug(thread_id);
                self.oracle_state.automatic_context_followups = 0;
                self.oracle_state.last_status = Some(message.clone());
                if matches!(kind, OracleRequestKind::Checkpoint) {
                    self.requeue_active_oracle_checkpoint(thread_id);
                }
                self.shutdown_oracle_broker_await(true).await;
                if matches!(kind, OracleRequestKind::UserTurn)
                    && self.oracle_state.pending_turn_id.is_some()
                {
                    self.complete_pending_oracle_turn_without_reply(thread_id)
                        .await;
                }
                self.record_oracle_status_event_for_run_kind(thread_id, kind, &message)
                    .await;
                self.persist_active_oracle_binding();
                Ok(true)
            }
            OracleSupervisorPhase::WaitingForOrchestrator => {
                let orchestrator_thread_id = self.oracle_state.orchestrator_thread_id;
                let interrupt_succeeded =
                    if let Some(orchestrator_thread_id) = orchestrator_thread_id {
                        if let Some(turn_id) =
                            self.active_turn_id_for_thread(orchestrator_thread_id).await
                        {
                            match app_server
                                .turn_interrupt(orchestrator_thread_id, turn_id)
                                .await
                            {
                                Ok(()) => true,
                                Err(err) => {
                                    tracing::warn!(
                                        %orchestrator_thread_id,
                                        %err,
                                        "failed to interrupt orchestrator turn from oracle thread"
                                    );
                                    false
                                }
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                let message = if interrupt_succeeded {
                    "Oracle workflow interrupted. The active orchestrator turn was interrupted."
                        .to_string()
                } else if orchestrator_thread_id.is_some() {
                    "Oracle workflow interrupt was requested while waiting for the orchestrator, but Codex could not confirm a backend interrupt. The workflow remains attached until a completion or closure arrives."
                        .to_string()
                } else {
                    "Oracle workflow interrupt was requested while waiting for the orchestrator, but no orchestrator thread was attached. Oracle supervision remains in place until the state is refreshed."
                        .to_string()
                };
                if !interrupt_succeeded {
                    self.oracle_state.last_status = Some(message.clone());
                    self.append_oracle_status_event(thread_id, &message).await;
                    self.persist_active_oracle_binding();
                    return Ok(true);
                };
                self.oracle_state.phase = OracleSupervisorPhase::Idle;
                self.oracle_state.orchestrator_thread_id = None;
                self.oracle_state.automatic_context_followups = 0;
                self.clear_oracle_run_tracking();
                self.oracle_state.last_status = Some(message.clone());
                self.clear_oracle_orchestrator_binding(thread_id);
                if let Some(binding) = self.oracle_state.bindings.get_mut(&thread_id)
                    && let Some(workflow) = binding.workflow.as_mut()
                {
                    workflow.status = OracleWorkflowStatus::Idle;
                    workflow.last_blocker = Some(
                        "The active orchestrator turn was interrupted by the user.".to_string(),
                    );
                }
                self.update_active_oracle_workflow_status(
                    OracleWorkflowStatus::Idle,
                    None,
                    Some("The active orchestrator turn was interrupted by the user.".to_string()),
                );
                self.refresh_routed_oracle_thread_owners(thread_id);
                self.append_oracle_status_event(thread_id, &message).await;
                self.persist_active_oracle_binding();
                Ok(true)
            }
            OracleSupervisorPhase::Disabled | OracleSupervisorPhase::Idle => Ok(false),
        }
    }

    fn oracle_thread_session(&self, thread_id: ThreadId) -> ThreadSessionState {
        let config = self.chat_widget.config_ref();
        let thread_label = self.oracle_thread_label(thread_id);
        let mut session = self
            .primary_session_configured
            .clone()
            .unwrap_or(ThreadSessionState {
                thread_id,
                forked_from_id: self.primary_thread_id,
                fork_parent_title: self
                    .primary_session_configured
                    .as_ref()
                    .and_then(|session| session.thread_name.clone()),
                thread_name: Some(thread_label.clone()),
                model: self.oracle_state.model.model_id().to_string(),
                model_provider_id: "oracle-browser".to_string(),
                service_tier: config.service_tier,
                approval_policy: config.permissions.approval_policy.value(),
                approvals_reviewer: config.approvals_reviewer,
                sandbox_policy: config
                    .permissions
                    .legacy_sandbox_policy(config.cwd.as_path()),
                permission_profile: None,
                cwd: config.cwd.clone(),
                instruction_source_paths: Vec::new(),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                network_proxy: None,
                rollout_path: Some(PathBuf::new()),
            });
        session.thread_id = thread_id;
        session.forked_from_id = self.primary_thread_id;
        session.thread_name = Some(thread_label);
        session.model = format!("requested {}", self.oracle_state.model.display_name());
        session.model_provider_id = "oracle-browser".to_string();
        session.cwd = config.cwd.clone();
        session.instruction_source_paths = Vec::new();
        session.history_log_id = 0;
        session.history_entry_count = 0;
        session.network_proxy = None;
        session.rollout_path = Some(PathBuf::new());
        session
    }

    async fn sync_oracle_thread_session(&mut self, thread_id: ThreadId) {
        let session = self.oracle_thread_session(thread_id);
        if let Some(channel) = self.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.session = Some(session.clone());
        }
        if self.current_displayed_thread_id() == Some(thread_id) {
            self.chat_widget.handle_thread_session(session);
        }
    }

    async fn create_visible_oracle_thread(&mut self) -> ThreadId {
        let thread_id = ThreadId::new();
        let session = self.oracle_thread_session(thread_id);
        let channel = self.ensure_thread_channel(thread_id);
        {
            let mut store = channel.store.lock().await;
            store.set_session(session, Vec::new());
        }
        self.oracle_state.bindings.insert(
            thread_id,
            OracleThreadBinding {
                phase: OracleSupervisorPhase::Idle,
                ..Default::default()
            },
        );
        self.activate_oracle_binding(thread_id);
        self.upsert_agent_picker_thread(
            thread_id,
            Some(self.oracle_thread_label(thread_id)),
            Some("supervisor".to_string()),
            /*is_closed*/ false,
        );
        self.sync_oracle_thread_session(thread_id).await;
        self.persist_active_oracle_binding();
        thread_id
    }

    async fn ensure_visible_oracle_thread(&mut self) -> ThreadId {
        if let Some(active_thread_id) = self.active_thread_id
            && self.is_visible_oracle_thread(active_thread_id)
        {
            self.activate_oracle_binding(active_thread_id);
            return active_thread_id;
        }
        if let Some(thread_id) = self.oracle_state.oracle_thread_id
            && self.is_visible_oracle_thread(thread_id)
        {
            self.activate_oracle_binding(thread_id);
            return thread_id;
        }
        if self.oracle_state.bindings.len() == 1
            && let Some(thread_id) = self.oracle_state.bindings.keys().next().copied()
        {
            self.activate_oracle_binding(thread_id);
            return thread_id;
        }
        self.create_visible_oracle_thread().await
    }

    async fn ensure_oracle_entrypoint_thread(
        &mut self,
        tui: Option<&mut tui::Tui>,
        app_server: &mut AppServerSession,
    ) -> Result<ThreadId> {
        let thread_id = self.ensure_visible_oracle_thread().await;
        if let Some(tui) = tui
            && self.active_thread_id != Some(thread_id)
        {
            self.select_agent_thread(tui, app_server, thread_id).await?;
        }
        self.persist_active_oracle_binding();
        Ok(thread_id)
    }

    fn is_visible_oracle_thread(&self, thread_id: ThreadId) -> bool {
        self.oracle_state.bindings.contains_key(&thread_id)
    }

    #[cfg(test)]
    fn is_hidden_oracle_thread(&self, thread_id: ThreadId) -> bool {
        self.oracle_state.orchestrator_thread_id == Some(thread_id)
            || self.oracle_state.bindings.values().any(|binding| {
                binding.orchestrator_thread_id == Some(thread_id)
                    || binding
                        .workflow
                        .as_ref()
                        .is_some_and(|workflow| workflow.orchestrator_thread_id == Some(thread_id))
                    || binding.participants.values().any(|participant| {
                        participant.thread_id == Some(thread_id)
                            && participant.owned_by_oracle
                            && participant.visibility == OracleParticipantVisibility::Hidden
                    })
            })
    }

    fn oracle_picker_description(&self, thread_id: ThreadId) -> Option<String> {
        if !self.is_visible_oracle_thread(thread_id) {
            return None;
        }
        let binding = self.oracle_state.bindings.get(&thread_id)?;
        let session = binding
            .current_session_id
            .as_deref()
            .unwrap_or("not started");
        let phase = binding.phase.description();
        let conversation = binding
            .conversation_id
            .as_deref()
            .map_or("unknown", |value| value);
        Some(format!(
            "requested {} | {} | {phase} | session {session} | convo {conversation}",
            self.oracle_state.model.display_name(),
            Self::current_oracle_workflow_status_line(binding),
        ))
    }

    fn oracle_picker_thread_title(&self, thread_id: ThreadId) -> String {
        self.oracle_state
            .bindings
            .get(&thread_id)
            .and_then(|binding| binding.remote_title.as_deref())
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "Oracle".to_string())
    }

    fn agent_picker_subtitle(&self) -> String {
        let base = AgentNavigationState::picker_subtitle();
        if self.oracle_state.bindings.is_empty() {
            base
        } else {
            format!("{base} Enter on Oracle to browse workflow threads.")
        }
    }

    fn oracle_workflow_picker_subtitle(&self, oracle_thread_id: ThreadId) -> String {
        let label = self.oracle_thread_label(oracle_thread_id);
        let workflow = self
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .and_then(|binding| binding.workflow.as_ref())
            .map(|workflow| {
                format!(
                    "{} v{}",
                    workflow.status.label().replace('_', " "),
                    workflow.version
                )
            })
            .unwrap_or_else(|| "direct chat".to_string());
        format!("{label} · {workflow}")
    }

    fn oracle_workflow_picker_thread_name(
        title: Option<&str>,
        role: Option<&str>,
        fallback: &str,
    ) -> String {
        let title = title
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(fallback);
        let role = role
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .filter(|value| !value.eq_ignore_ascii_case(title));
        match role {
            Some(role) => format!("{title} [{role}]"),
            None => title.to_string(),
        }
    }

    fn oracle_workflow_picker_thread_description(
        &self,
        thread_id: ThreadId,
        label: &str,
        detail: &str,
    ) -> String {
        let state = if self
            .agent_navigation
            .get(&thread_id)
            .is_some_and(|entry| entry.is_closed)
        {
            "closed"
        } else {
            "open"
        };
        format!("{label} | {detail} | {state}")
    }

    fn open_oracle_workflow_picker(&mut self, oracle_thread_id: ThreadId) {
        let Some(binding) = self.oracle_state.bindings.get(&oracle_thread_id).cloned() else {
            self.chat_widget.add_error_message(format!(
                "Oracle thread {oracle_thread_id} is no longer available."
            ));
            return;
        };

        let mut initial_selected_idx = None;
        let mut items = Vec::new();
        let mut seen_thread_ids = HashSet::from([oracle_thread_id]);

        let supervisor_label = self.oracle_thread_label(oracle_thread_id);
        let supervisor_name = Self::oracle_workflow_picker_thread_name(
            Some(supervisor_label.as_str()),
            Some("supervisor"),
            "Oracle",
        );
        let supervisor_description = self.oracle_workflow_picker_thread_description(
            oracle_thread_id,
            "Supervisor thread",
            "direct Oracle conversation",
        );
        if self.active_thread_id == Some(oracle_thread_id) {
            initial_selected_idx = Some(items.len());
        }
        items.push(SelectionItem {
            name: supervisor_name.clone(),
            name_prefix_spans: agent_picker_status_dot_spans(
                self.agent_navigation
                    .get(&oracle_thread_id)
                    .is_some_and(|entry| entry.is_closed),
            ),
            description: Some(supervisor_description.clone()),
            is_current: self.active_thread_id == Some(oracle_thread_id),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::SelectAgentThread(oracle_thread_id));
            })],
            dismiss_on_select: true,
            search_value: Some(format!(
                "{supervisor_name} {oracle_thread_id} {supervisor_description}"
            )),
            ..Default::default()
        });

        if let Some(orchestrator_thread_id) = binding.orchestrator_thread_id
            && seen_thread_ids.insert(orchestrator_thread_id)
        {
            let name = Self::oracle_workflow_picker_thread_name(
                Some("Oracle Orchestrator"),
                Some("orchestrator"),
                "Oracle Orchestrator",
            );
            let description = self.oracle_workflow_picker_thread_description(
                orchestrator_thread_id,
                "Workflow thread",
                "supervised orchestrator",
            );
            if self.active_thread_id == Some(orchestrator_thread_id) {
                initial_selected_idx = Some(items.len());
            }
            items.push(SelectionItem {
                name: name.clone(),
                name_prefix_spans: agent_picker_status_dot_spans(
                    self.agent_navigation
                        .get(&orchestrator_thread_id)
                        .is_some_and(|entry| entry.is_closed),
                ),
                description: Some(description.clone()),
                is_current: self.active_thread_id == Some(orchestrator_thread_id),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::SelectAgentThread(orchestrator_thread_id));
                })],
                dismiss_on_select: true,
                search_value: Some(format!("{name} {orchestrator_thread_id} {description}")),
                ..Default::default()
            });
        }

        let mut participants = binding.participants.values().cloned().collect::<Vec<_>>();
        participants.sort_by(|a, b| {
            a.title
                .as_deref()
                .unwrap_or(a.address.as_str())
                .cmp(b.title.as_deref().unwrap_or(b.address.as_str()))
        });
        for participant in participants {
            if participant.address == ORACLE_HUMAN_DESTINATION {
                continue;
            }
            let Some(thread_id) = participant.thread_id else {
                continue;
            };
            if !seen_thread_ids.insert(thread_id) {
                continue;
            }
            let name = Self::oracle_workflow_picker_thread_name(
                participant.title.as_deref(),
                participant.role.as_deref().or(participant.kind.as_deref()),
                participant.address.as_str(),
            );
            let detail = if participant.visibility == OracleParticipantVisibility::Hidden {
                "linked hidden workflow thread"
            } else {
                "linked workflow thread"
            };
            let description = self.oracle_workflow_picker_thread_description(
                thread_id,
                "Workflow thread",
                detail,
            );
            if self.active_thread_id == Some(thread_id) {
                initial_selected_idx = Some(items.len());
            }
            items.push(SelectionItem {
                name: name.clone(),
                name_prefix_spans: agent_picker_status_dot_spans(
                    self.agent_navigation
                        .get(&thread_id)
                        .is_some_and(|entry| entry.is_closed),
                ),
                description: Some(description.clone()),
                is_current: self.active_thread_id == Some(thread_id),
                actions: vec![Box::new(move |tx| {
                    tx.send(AppEvent::SelectAgentThread(thread_id));
                })],
                dismiss_on_select: true,
                search_value: Some(format!("{name} {thread_id} {description}")),
                ..Default::default()
            });
        }

        self.chat_widget.show_selection_view(SelectionViewParams {
            view_id: Some(ORACLE_WORKFLOW_SELECTION_VIEW_ID),
            title: Some("Oracle Workflow".to_string()),
            subtitle: Some(self.oracle_workflow_picker_subtitle(oracle_thread_id)),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            initial_selected_idx,
            ..Default::default()
        });
    }

    fn oracle_picker_subtitle(&self, attached_count: usize, remote_count: usize) -> String {
        if self.oracle_picker_show_info {
            format!(
                "Model: {}  Phase: {}  Attached: {}  Remote: {}",
                self.oracle_state.model.picker_label(),
                self.oracle_state.phase.description(),
                attached_count,
                remote_count,
            )
        } else if self.oracle_picker_remote_list_pending {
            "Fetching remote Oracle threads in the background. Pick a local thread or use the hotkeys below.".to_string()
        } else if self.oracle_picker_remote_list_notice.is_some() {
            "Remote Oracle threads are unavailable. Local controls are still available.".to_string()
        } else if attached_count + remote_count == 0 {
            "No Oracle threads yet. Use the hotkeys below.".to_string()
        } else {
            "Pick a thread or use the hotkeys below.".to_string()
        }
    }

    fn oracle_picker_hotkeys_line(
        &self,
        include_new_thread: bool,
        search_enabled: bool,
    ) -> Line<'static> {
        let next_model = self.oracle_state.model.toggle_family();
        let mut spans = vec!["Hotkeys: ".dim()];
        if include_new_thread {
            spans.push(key_hint::plain(KeyCode::Char('n')).into());
            spans.push(" new  ".dim());
        }
        if search_enabled {
            spans.push(key_hint::plain(KeyCode::Char('s')).into());
            spans.push(" search  ".dim());
        }
        spans.push(key_hint::plain(KeyCode::Char('i')).into());
        spans.push(" info  ".dim());
        spans.push(key_hint::plain(KeyCode::Tab).into());
        spans.push(format!(" {}", next_model.browser_label().to_ascii_lowercase()).dim());
        Line::from(spans)
    }

    fn refresh_oracle_picker_if_open(&mut self) -> bool {
        if !self
            .chat_widget
            .selection_view_is_active(ORACLE_SELECTION_VIEW_ID)
        {
            return false;
        }
        self.show_oracle_picker(
            self.oracle_picker_remote_threads.clone(),
            self.oracle_picker_include_new_thread,
        );
        true
    }

    fn toggle_oracle_picker_info(&mut self) -> bool {
        if !self
            .chat_widget
            .selection_view_is_active(ORACLE_SELECTION_VIEW_ID)
        {
            return false;
        }
        self.oracle_picker_show_info = !self.oracle_picker_show_info;
        self.refresh_oracle_picker_if_open()
    }

    async fn toggle_oracle_picker_model(&mut self) -> bool {
        if !self
            .chat_widget
            .selection_view_is_active(ORACLE_SELECTION_VIEW_ID)
        {
            return false;
        }
        let next_model = self.oracle_state.model.toggle_family();
        let _ = self.set_oracle_model_preference(next_model).await;
        self.refresh_oracle_picker_if_open()
    }

    fn open_oracle_model_popup(&mut self) {
        let items = [
            (
                true,
                "GPT-5.5 Pro",
                "Best Oracle reasoning quality. Press enter to choose Standard or Extended.",
            ),
            (
                false,
                "Thinking 5.5",
                "Faster, cheaper Oracle turns when you want a lighter pass.",
            ),
        ]
        .into_iter()
        .map(|(is_pro, name, description)| SelectionItem {
            name: name.to_string(),
            description: Some(description.to_string()),
            is_current: if is_pro {
                self.oracle_state.model.is_pro()
            } else {
                self.oracle_state.model == OracleModelPreset::Thinking
            },
            actions: vec![Box::new(move |tx| {
                if is_pro {
                    tx.send(AppEvent::OpenOracleProReasoningPopup);
                } else {
                    tx.send(AppEvent::ConfigureOracleMode {
                        raw_command: "model thinking".to_string(),
                    });
                }
            })],
            dismiss_on_select: !is_pro,
            ..Default::default()
        })
        .collect();

        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Oracle Model".to_string()),
            subtitle: Some(format!(
                "Current Oracle model: {}",
                self.oracle_state.model.picker_label()
            )),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    fn open_oracle_pro_reasoning_popup(&mut self) {
        let items = [
            (
                OracleModelPreset::Pro,
                "Standard",
                "Use standard Pro thinking for the normal high-quality Oracle path.",
            ),
            (
                OracleModelPreset::ProExtended,
                "Extended",
                "Use extended Pro thinking when Oracle should spend more time reasoning.",
            ),
        ]
        .into_iter()
        .map(|(model, name, description)| SelectionItem {
            name: name.to_string(),
            description: Some(description.to_string()),
            is_current: self.oracle_state.model == model,
            actions: vec![Box::new(move |tx| {
                let raw_command = match model {
                    OracleModelPreset::Pro => "model pro standard",
                    OracleModelPreset::ProExtended => "model pro extended",
                    OracleModelPreset::Thinking => "model thinking",
                };
                tx.send(AppEvent::ConfigureOracleMode {
                    raw_command: raw_command.to_string(),
                });
            })],
            dismiss_on_select: true,
            ..Default::default()
        })
        .collect();

        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Oracle Pro Thinking".to_string()),
            subtitle: Some(format!(
                "Current Oracle model: {}",
                self.oracle_state.model.picker_label()
            )),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    fn open_contextual_model_popup(&mut self) {
        if self
            .current_displayed_thread_id()
            .is_some_and(|thread_id| self.is_visible_oracle_thread(thread_id))
        {
            self.open_oracle_model_popup();
        } else {
            self.chat_widget.open_model_popup();
        }
    }

    fn oracle_browser_model_strategy(&self) -> &'static str {
        "select"
    }

    fn oracle_thread_is_displayed(&self, thread_id: ThreadId) -> bool {
        self.current_displayed_thread_id() == Some(thread_id)
    }

    fn oracle_thread_history_limit(&self) -> Option<usize> {
        std::env::var("CODEX_ORACLE_THREAD_HISTORY_LIMIT")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
            .or(Some(100))
    }

    fn oracle_thread_history_requires_reattach(error: &str) -> bool {
        let normalized = error.to_ascii_lowercase();
        (normalized.contains("session") || normalized.contains("runtime"))
            && (normalized.contains("was not found")
                || normalized.contains("could not be found")
                || normalized.contains("not reusable yet")
                || normalized.contains("missing browser metadata"))
    }

    fn oracle_attach_history_message(
        binding: &OracleAttachThreadBinding,
        outcome: OracleHistoryImportOutcome,
        history_window: Option<&OracleBrokerThreadHistoryWindow>,
    ) -> String {
        let base = match binding {
            OracleAttachThreadBinding::ReattachedLocal {
                thread_id, title, ..
            } => {
                format!("Reattached existing local Oracle thread {thread_id}: {title}")
            }
            OracleAttachThreadBinding::AttachedRemote {
                thread_id,
                title,
                conversation_id,
                ..
            } => {
                format!("Attached Oracle thread on {thread_id}: {title} ({conversation_id})")
            }
        };
        let note = Self::oracle_history_window_note(history_window);
        match outcome {
            OracleHistoryImportOutcome::Imported { message_count } => {
                format!("{base}. Imported {message_count} prior message(s).{note}")
            }
            OracleHistoryImportOutcome::NoNewMessages => {
                format!("{base}. No new remote history needed importing.{note}")
            }
            OracleHistoryImportOutcome::SkippedToAvoidConflict => {
                format!(
                    "{base}. Skipped remote history import because the local transcript already diverged.{note}"
                )
            }
        }
    }

    fn oracle_history_window_note(
        history_window: Option<&OracleBrokerThreadHistoryWindow>,
    ) -> String {
        let Some(history_window) = history_window else {
            return String::new();
        };
        if !history_window.truncated || history_window.total_count <= history_window.returned_count
        {
            return String::new();
        }
        format!(
            " Remote history was limited to the newest {} text message(s) out of at least {} available.",
            history_window.returned_count, history_window.total_count
        )
    }

    fn oracle_history_import_requires_snapshot_refresh(
        has_buffered_events: bool,
        merged_into_existing_local_turn: bool,
    ) -> bool {
        has_buffered_events || merged_into_existing_local_turn
    }

    async fn push_oracle_turn(&mut self, thread_id: ThreadId, turn: Turn) {
        let should_render = self.oracle_thread_is_displayed(thread_id);
        let channel = self.ensure_thread_channel(thread_id);
        {
            let mut store = channel.store.lock().await;
            if matches!(turn.status, TurnStatus::InProgress) {
                store.active_turn_id = Some(turn.id.clone());
            } else if store.active_turn_id.as_deref() == Some(turn.id.as_str()) {
                store.active_turn_id = None;
            }
            store.turns.push(turn.clone());
            store.push_turn_replay(turn.clone());
        }
        if should_render {
            self.chat_widget
                .replay_thread_turns(vec![turn], ReplayKind::ThreadSnapshot);
        }
    }

    fn normalize_oracle_history_entries(
        history: Vec<OracleBrokerThreadHistoryEntry>,
    ) -> Vec<OracleBrokerThreadHistoryEntry> {
        history
            .into_iter()
            .filter_map(|entry| {
                let role = entry.role.trim().to_ascii_lowercase();
                let text = entry.text.trim();
                if !matches!(role.as_str(), "user" | "assistant") || text.is_empty() {
                    return None;
                }
                Some(OracleBrokerThreadHistoryEntry {
                    role,
                    text: text.to_string(),
                })
            })
            .collect()
    }

    fn oracle_turns_to_history_entries(turns: &[Turn]) -> Vec<OracleBrokerThreadHistoryEntry> {
        let mut history = Vec::new();
        for turn in turns {
            for item in &turn.items {
                match item {
                    ThreadItem::UserMessage { content, .. } => {
                        let text = content
                            .iter()
                            .filter_map(|item| match item {
                                codex_app_server_protocol::UserInput::Text { text, .. } => {
                                    Some(text.trim())
                                }
                                _ => None,
                            })
                            .filter(|text| !text.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        if !text.is_empty() {
                            history.push(OracleBrokerThreadHistoryEntry {
                                role: "user".to_string(),
                                text,
                            });
                        }
                    }
                    ThreadItem::AgentMessage { text, .. } => {
                        let text = text.trim();
                        if !text.is_empty() {
                            history.push(OracleBrokerThreadHistoryEntry {
                                role: "assistant".to_string(),
                                text: text.to_string(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        history
    }

    fn oracle_turns_have_nontext_user_inputs(turns: &[Turn]) -> bool {
        turns.iter().any(|turn| {
            turn.items.iter().any(|item| {
                matches!(
                    item,
                    ThreadItem::UserMessage { content, .. }
                        if content.iter().any(|input| {
                            !matches!(
                                input,
                                codex_app_server_protocol::UserInput::Text { .. }
                            )
                        })
                )
            })
        })
    }

    fn oracle_missing_history_suffix<'a>(
        existing: &[OracleBrokerThreadHistoryEntry],
        remote: &'a [OracleBrokerThreadHistoryEntry],
    ) -> Option<&'a [OracleBrokerThreadHistoryEntry]> {
        if existing.is_empty() {
            return Some(remote);
        }
        if remote.is_empty() {
            return Some(&[]);
        }
        let max_overlap = existing.len().min(remote.len());
        for overlap in (1..=max_overlap).rev() {
            if existing[existing.len() - overlap..] == remote[..overlap] {
                if overlap == 1
                    && (existing.len() != 1
                        || existing[0].role != "user"
                        || remote[0].role != "user")
                {
                    continue;
                }
                return Some(&remote[overlap..]);
            }
        }
        None
    }

    fn oracle_history_entries_to_turns(history: &[OracleBrokerThreadHistoryEntry]) -> Vec<Turn> {
        let mut turns = Vec::new();
        let mut pending_user_turn: Option<Turn> = None;
        for entry in history {
            match entry.role.as_str() {
                "user" => {
                    if let Some(turn) = pending_user_turn.take() {
                        turns.push(turn);
                    }
                    pending_user_turn = Some(Turn {
                        id: format!("oracle-turn-import-{}", Uuid::new_v4()),
                        items: vec![ThreadItem::UserMessage {
                            id: format!("oracle-user-import-{}", Uuid::new_v4()),
                            content: vec![codex_app_server_protocol::UserInput::Text {
                                text: entry.text.clone(),
                                text_elements: Vec::new(),
                            }],
                        }],
                        status: TurnStatus::Completed,
                        error: None,
                        started_at: None,
                        completed_at: None,
                        duration_ms: None,
                    });
                }
                "assistant" => {
                    let agent_item = ThreadItem::AgentMessage {
                        id: format!("oracle-assistant-import-{}", Uuid::new_v4()),
                        text: entry.text.clone(),
                        phase: None,
                        memory_citation: None,
                    };
                    if let Some(mut turn) = pending_user_turn.take() {
                        turn.items.push(agent_item);
                        turns.push(turn);
                    } else {
                        turns.push(Turn {
                            id: format!("oracle-turn-import-{}", Uuid::new_v4()),
                            items: vec![agent_item],
                            status: TurnStatus::Completed,
                            error: None,
                            started_at: None,
                            completed_at: None,
                            duration_ms: None,
                        });
                    }
                }
                _ => {}
            }
        }
        if let Some(turn) = pending_user_turn.take() {
            turns.push(turn);
        }
        turns
    }

    async fn replay_oracle_thread_snapshot(
        &mut self,
        tui: &mut tui::Tui,
        thread_id: ThreadId,
    ) -> Result<()> {
        let snapshot = {
            let Some(channel) = self.thread_event_channels.get(&thread_id) else {
                return Ok(());
            };
            let store = channel.store.lock().await;
            store.snapshot()
        };
        let init = self.chatwidget_init_for_forked_or_resumed_thread(
            tui,
            self.config.clone(),
            /*initial_user_message*/ None,
        );
        self.replace_chat_widget(ChatWidget::new_with_app_event(init));
        self.reset_for_thread_switch(tui)?;
        self.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);
        Ok(())
    }

    async fn import_oracle_thread_history(
        &mut self,
        tui: Option<&mut tui::Tui>,
        thread_id: ThreadId,
        conversation_id: &str,
        history: Vec<OracleBrokerThreadHistoryEntry>,
    ) -> Result<OracleHistoryImportOutcome> {
        let bound_conversation_id = self
            .oracle_state
            .bindings
            .get(&thread_id)
            .and_then(|binding| binding.conversation_id.as_deref());
        if let Some(bound_conversation_id) = bound_conversation_id
            && bound_conversation_id != conversation_id
        {
            return Err(color_eyre::eyre::eyre!(
                "Oracle thread {thread_id} is bound to remote conversation {bound_conversation_id}, so importing history for {conversation_id} would cross streams."
            ));
        }
        let remote_history = Self::normalize_oracle_history_entries(history);
        let (existing_history, has_buffered_events) =
            if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                let store = channel.store.lock().await;
                if Self::oracle_turns_have_nontext_user_inputs(&store.turns) {
                    return Ok(OracleHistoryImportOutcome::SkippedToAvoidConflict);
                }
                (
                    Self::oracle_turns_to_history_entries(&store.turns),
                    !store.buffer.is_empty(),
                )
            } else {
                (Vec::new(), false)
            };
        let Some(missing_history) =
            Self::oracle_missing_history_suffix(&existing_history, &remote_history)
        else {
            return Ok(OracleHistoryImportOutcome::SkippedToAvoidConflict);
        };
        if missing_history.is_empty() {
            self.persist_active_oracle_binding();
            return Ok(OracleHistoryImportOutcome::NoNewMessages);
        }
        let mut imported_turns = Self::oracle_history_entries_to_turns(missing_history);
        let message_count = missing_history.len();
        let mut merged_into_existing_local_turn = false;
        {
            let channel = self.ensure_thread_channel(thread_id);
            let mut store = channel.store.lock().await;
            let mut merged_turns = store.turns.clone();
            if missing_history
                .first()
                .is_some_and(|entry| entry.role == "assistant")
                && merged_turns.last().is_some_and(|turn| {
                    turn.items
                        .iter()
                        .any(|item| matches!(item, ThreadItem::UserMessage { .. }))
                        && !turn
                            .items
                            .iter()
                            .any(|item| matches!(item, ThreadItem::AgentMessage { .. }))
                })
                && imported_turns.first().is_some_and(|turn| {
                    turn.items
                        .iter()
                        .all(|item| matches!(item, ThreadItem::AgentMessage { .. }))
                })
            {
                if let Some(first_imported_turn) = imported_turns.first()
                    && let Some(last_turn) = merged_turns.last_mut()
                {
                    last_turn.items.extend(first_imported_turn.items.clone());
                    merged_into_existing_local_turn = true;
                }
                imported_turns.remove(0);
            }
            merged_turns.extend(imported_turns.clone());
            store.set_turns(merged_turns);
            store.reseed_replay_entries_from_state();
        }
        self.persist_active_oracle_binding();
        if self.oracle_thread_is_displayed(thread_id) {
            if Self::oracle_history_import_requires_snapshot_refresh(
                has_buffered_events,
                merged_into_existing_local_turn,
            ) {
                if let Some(tui) = tui {
                    self.replay_oracle_thread_snapshot(tui, thread_id).await?;
                }
            } else {
                self.chat_widget
                    .replay_thread_turns(imported_turns, ReplayKind::ThreadSnapshot);
            }
        }
        Ok(OracleHistoryImportOutcome::Imported { message_count })
    }

    async fn begin_oracle_user_turn(&mut self, thread_id: ThreadId, items: &[UserInput]) {
        self.activate_oracle_binding(thread_id);
        let turn_id = format!("oracle-turn-{}", Uuid::new_v4());
        self.oracle_state.pending_turn_id = Some(turn_id.clone());
        let channel = self.ensure_thread_channel(thread_id);
        {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some(turn_id.clone());
            let turn = Turn {
                id: turn_id.clone(),
                items: vec![ThreadItem::UserMessage {
                    id: format!("oracle-user-{}", Uuid::new_v4()),
                    content: items
                        .iter()
                        .cloned()
                        .map(codex_app_server_protocol::UserInput::from)
                        .collect(),
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            };
            store.turns.push(turn.clone());
            store.push_turn_replay(turn);
        }
        if self.oracle_thread_is_displayed(thread_id) {
            self.chat_widget.replay_thread_turns(
                vec![Turn {
                    id: turn_id,
                    items: Vec::new(),
                    status: TurnStatus::InProgress,
                    error: None,
                    started_at: None,
                    completed_at: None,
                    duration_ms: None,
                }],
                ReplayKind::ThreadSnapshot,
            );
        }
        self.persist_active_oracle_binding();
    }

    async fn append_oracle_agent_turn(&mut self, thread_id: ThreadId, message: &str) {
        if message.trim().is_empty() {
            return;
        }
        self.push_oracle_turn(
            thread_id,
            Turn {
                id: format!("oracle-turn-{}", Uuid::new_v4()),
                items: vec![ThreadItem::AgentMessage {
                    id: format!("oracle-assistant-{}", Uuid::new_v4()),
                    text: message.trim().to_string(),
                    phase: None,
                    memory_citation: None,
                }],
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            },
        )
        .await;
    }

    async fn complete_pending_oracle_turn(&mut self, thread_id: ThreadId, message: &str) {
        self.complete_pending_oracle_turn_with_optional_reply(thread_id, Some(message))
            .await;
    }

    async fn complete_pending_oracle_turn_without_reply(&mut self, thread_id: ThreadId) {
        self.complete_pending_oracle_turn_with_optional_reply(thread_id, None)
            .await;
    }

    async fn complete_pending_oracle_turn_with_optional_reply(
        &mut self,
        thread_id: ThreadId,
        message: Option<&str>,
    ) {
        self.activate_oracle_binding(thread_id);
        let trimmed_message = message.map(str::trim).filter(|message| !message.is_empty());
        let Some(turn_id) = self.oracle_state.pending_turn_id.take() else {
            if let Some(message) = trimmed_message {
                self.append_oracle_agent_turn(thread_id, message).await;
            }
            return;
        };
        let agent_item = trimmed_message.map(|message| ThreadItem::AgentMessage {
            id: format!("oracle-assistant-{}", Uuid::new_v4()),
            text: message.to_string(),
            phase: None,
            memory_citation: None,
        });
        let should_render = self.oracle_thread_is_displayed(thread_id);
        let mut completed_turn = None;
        if let Some(channel) = self.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            let replay_turn =
                if let Some(turn) = store.turns.iter_mut().rev().find(|turn| turn.id == turn_id) {
                    if let Some(agent_item) = &agent_item {
                        turn.items.push(agent_item.clone());
                    }
                    turn.status = TurnStatus::Completed;
                    turn.error = None;
                    turn.clone()
                } else {
                    let turn = Turn {
                        id: turn_id.clone(),
                        items: agent_item.iter().cloned().collect(),
                        status: TurnStatus::Completed,
                        error: None,
                        started_at: None,
                        completed_at: None,
                        duration_ms: None,
                    };
                    store.turns.push(turn.clone());
                    turn
                };
            store.push_turn_replay(replay_turn.clone());
            if store.active_turn_id.as_deref() == Some(turn_id.as_str()) {
                store.active_turn_id = None;
            }
            completed_turn = Some(replay_turn);
        }
        if should_render && let Some(turn) = completed_turn {
            self.chat_widget
                .replay_thread_turns(vec![turn], ReplayKind::ThreadSnapshot);
        }
        self.persist_active_oracle_binding();
    }

    fn handle_oracle_workflow_thread_event(&mut self, event: OracleWorkflowThreadEvent) {
        self.chat_widget
            .add_to_history(system_event_cell(event.title, event.details));
    }

    async fn append_oracle_workflow_event(
        &mut self,
        thread_id: ThreadId,
        event: OracleWorkflowThreadEvent,
    ) {
        if event.title.trim().is_empty() {
            return;
        }
        self.enqueue_thread_oracle_workflow_event(thread_id, event)
            .await;
    }

    async fn append_oracle_status_event(&mut self, thread_id: ThreadId, message: &str) {
        self.append_oracle_workflow_event(thread_id, Self::oracle_status_thread_event(message))
            .await;
    }

    async fn complete_pending_oracle_turn_with_status_event(
        &mut self,
        thread_id: ThreadId,
        message: &str,
    ) {
        if message.trim().is_empty() {
            return;
        }
        self.complete_pending_oracle_turn_without_reply(thread_id)
            .await;
        self.append_oracle_status_event(thread_id, message).await;
    }

    async fn record_oracle_status_event_for_run_kind(
        &mut self,
        thread_id: ThreadId,
        kind: OracleRequestKind,
        message: &str,
    ) {
        if matches!(kind, OracleRequestKind::UserTurn) {
            self.complete_pending_oracle_turn_with_status_event(thread_id, message)
                .await;
        } else {
            self.append_oracle_status_event(thread_id, message).await;
        }
    }

    async fn handle_oracle_user_turn(
        &mut self,
        _app_server: &mut AppServerSession,
        thread_id: ThreadId,
        items: &[UserInput],
    ) -> Result<bool> {
        if !self.oracle_state.intercepts(thread_id) {
            return Ok(false);
        }
        self.activate_oracle_binding(thread_id);
        if self.oracle_state.phase.is_busy() {
            self.chat_widget.add_error_message(format!(
                "Oracle is already busy: {}.",
                self.oracle_state.phase.description()
            ));
            self.persist_active_oracle_binding();
            return Ok(true);
        }
        if self.oracle_browser_is_busy() {
            self.chat_widget.add_error_message(
                "Oracle is already using the shared browser in another thread. Wait for that turn to finish before starting a new Oracle turn."
                    .to_string(),
            );
            self.persist_active_oracle_binding();
            return Ok(true);
        }
        let source_user_text = Self::flatten_user_turn_source_text(items);
        let prompt_seed = Self::flatten_user_turn_text(items);
        let workspace_cwd = self.chat_widget.config_ref().cwd.to_path_buf();
        let attachment_resolution =
            match resolve_oracle_attachments(&prompt_seed, items, workspace_cwd.as_path()).await {
                Ok(resolved) => resolved,
                Err(err) => {
                    self.chat_widget
                        .add_error_message(format!("Oracle attachment setup failed: {err}"));
                    self.persist_active_oracle_binding();
                    return Ok(true);
                }
            };
        let unsupported = Self::oracle_unsupported_user_inputs(items);
        if !unsupported.is_empty() {
            self.chat_widget.add_info_message(
                format!(
                    "Oracle currently forwards {} as text markers. Local images and explicit file:/glob: lines are sent as files.",
                    unsupported.join(", ")
                ),
                /*hint*/ None,
            );
        }
        let prompt_text = attachment_resolution.prompt;
        if source_user_text.trim().is_empty() {
            self.chat_widget.add_error_message(
                "Oracle mode currently supports text-first turns only.".to_string(),
            );
            self.persist_active_oracle_binding();
            return Ok(true);
        }
        let Some(oracle_repo) = find_oracle_repo(self.chat_widget.config_ref().cwd.as_path())
        else {
            let message = Self::oracle_repo_not_found_message().to_string();
            self.oracle_state.last_status = Some(message.clone());
            self.chat_widget.add_error_message(message);
            self.persist_active_oracle_binding();
            return Ok(true);
        };
        let (session_slug, followup_session) = match self
            .ensure_oracle_session_continuity_for_run(thread_id)
            .await
        {
            Ok(continuity) => continuity,
            Err(err) => {
                let message = format!("Oracle could not reattach the hidden browser thread: {err}");
                self.oracle_state.phase = OracleSupervisorPhase::Idle;
                self.oracle_state.last_status = Some(message.clone());
                self.chat_widget.add_error_message(message);
                self.persist_active_oracle_binding();
                return Ok(true);
            }
        };
        self.oracle_state.last_status = Some(format!(
            "Waiting for Oracle ({}) on thread {thread_id}.",
            self.oracle_state.model.display_name()
        ));
        self.oracle_state.automatic_context_followups = 0;
        self.reopen_active_oracle_workflow_for_user_turn();
        self.begin_oracle_user_turn(thread_id, items).await;
        let requires_control = self.oracle_workflow_requires_control()
            || user_turn_requires_orchestrator_control(&source_user_text);
        let oracle_prompt =
            build_user_turn_prompt(&self.oracle_state, &prompt_text, requires_control);
        if let Err(err) = self.start_oracle_run(OracleRunRequest {
            oracle_thread_id: thread_id,
            kind: OracleRequestKind::UserTurn,
            session_slug,
            prompt: oracle_prompt.clone(),
            requested_prompt: oracle_prompt,
            source_user_text: Some(source_user_text),
            files: attachment_resolution.files,
            workspace_cwd,
            oracle_repo,
            followup_session,
            model: self.oracle_state.model,
            browser_model_strategy: self.oracle_browser_model_strategy().to_string(),
            browser_model_label: Some(self.oracle_state.model.browser_label().to_string()),
            browser_thinking_time: self
                .oracle_state
                .model
                .browser_thinking_time()
                .map(str::to_string),
            requires_control,
            repair_attempt: 0,
            transport_retry_attempt: 0,
        }) {
            let message = format!("Oracle failed before the run started: {err}");
            self.oracle_state.phase = OracleSupervisorPhase::Idle;
            self.oracle_state.last_status = Some(message.clone());
            self.complete_pending_oracle_turn(thread_id, &message).await;
            self.chat_widget
                .add_error_message(format!("Oracle supervisor failed: {err}"));
        }
        self.persist_active_oracle_binding();
        Ok(true)
    }

    async fn ensure_orchestrator_thread(
        &mut self,
        app_server: &mut AppServerSession,
    ) -> Result<ThreadId> {
        if let Some(thread_id) = self
            .oracle_state
            .workflow
            .as_ref()
            .and_then(|workflow| workflow.orchestrator_thread_id)
            .or(self.oracle_state.orchestrator_thread_id)
        {
            if self.thread_event_channels.contains_key(&thread_id) {
                self.oracle_state.orchestrator_thread_id = Some(thread_id);
                return Ok(thread_id);
            }
            tracing::info!(
                %thread_id,
                "discarding closed oracle orchestrator thread before starting a new one"
            );
            self.oracle_state.orchestrator_thread_id = None;
            if let Some(workflow) = self.oracle_state.workflow.as_mut()
                && workflow.orchestrator_thread_id == Some(thread_id)
            {
                workflow.orchestrator_thread_id = None;
            }
            self.persist_active_oracle_binding();
        }
        let Some(_oracle_thread_id) = self.oracle_state.oracle_thread_id else {
            return Err(color_eyre::eyre::eyre!(
                "oracle orchestrator requested without an active oracle thread binding"
            ));
        };
        let started = app_server
            .start_thread(self.chat_widget.config_ref())
            .await?;
        let thread_id = started.session.thread_id;
        let channel = self.ensure_thread_channel(thread_id);
        {
            let mut store = channel.store.lock().await;
            store.set_session(started.session, started.turns);
        }
        if let Err(err) = app_server
            .thread_set_name(thread_id, "Oracle Orchestrator".to_string())
            .await
        {
            tracing::warn!(%thread_id, %err, "failed to name oracle orchestrator thread");
        }
        self.oracle_state.orchestrator_thread_id = Some(thread_id);
        if let Some(workflow) = self.oracle_state.workflow.as_mut() {
            workflow.orchestrator_thread_id = Some(thread_id);
        }
        self.persist_active_oracle_binding();
        Ok(thread_id)
    }

    async fn oracle_destination_task_op(
        &self,
        oracle_thread_id: ThreadId,
        destination_thread_id: ThreadId,
        _address: &str,
        _role: Option<&str>,
        task: String,
        developer_instructions: Option<String>,
    ) -> AppCommand {
        let routed_model = self
            .oracle_destination_task_model(destination_thread_id)
            .await;
        let collaboration_mode = self
            .chat_widget
            .submission_collaboration_mode()
            .unwrap_or_else(|| self.chat_widget.current_collaboration_mode().clone())
            .with_updates(
                Some(routed_model.clone()),
                /*effort*/ None,
                Some(developer_instructions),
            );
        let personality = self.chat_widget.config_ref().personality.filter(|_| {
            self.chat_widget
                .config_ref()
                .features
                .enabled(Feature::Personality)
        });
        let config = self.chat_widget.config_ref();
        let sandbox_policy = config
            .permissions
            .legacy_sandbox_policy(config.cwd.as_path());
        let permission_profile = if matches!(&sandbox_policy, SandboxPolicy::ExternalSandbox { .. })
        {
            None
        } else {
            Some(config.permissions.permission_profile())
        };
        let reminder = self
            .oracle_state
            .workflow
            .as_ref()
            .map(|workflow| build_oracle_workflow_reminder(workflow, oracle_thread_id, &task))
            .unwrap_or(task);
        AppCommand::user_turn(
            vec![UserInput::Text {
                text: reminder,
                text_elements: Vec::new(),
            }],
            config.cwd.to_path_buf(),
            config.permissions.approval_policy.value(),
            sandbox_policy,
            permission_profile,
            routed_model,
            collaboration_mode.reasoning_effort(),
            /*summary*/ None,
            config.service_tier.map(Some),
            /*final_output_json_schema*/ None,
            Some(collaboration_mode),
            personality,
        )
    }

    async fn oracle_destination_task_model(&self, destination_thread_id: ThreadId) -> String {
        if let Some(channel) = self.thread_event_channels.get(&destination_thread_id) {
            let store = channel.store.lock().await;
            if let Some(session) = store.session.as_ref()
                && session.model_provider_id != "oracle-browser"
                && !session.model.trim().is_empty()
            {
                return session.model.clone();
            }
        }

        if let Some(session) = self.primary_session_configured.as_ref()
            && session.model_provider_id != "oracle-browser"
            && !session.model.trim().is_empty()
        {
            return session.model.clone();
        }

        if let Some(model) = self
            .chat_widget
            .config_ref()
            .model
            .clone()
            .filter(|model| !model.trim().is_empty())
        {
            return model;
        }

        if let Some(model) = self
            .config
            .model
            .clone()
            .filter(|model| !model.trim().is_empty())
        {
            return model;
        }

        self.chat_widget
            .current_collaboration_mode()
            .model()
            .to_string()
    }

    async fn maybe_process_pending_oracle_checkpoints(
        &mut self,
        app_server: &mut AppServerSession,
        oracle_thread_id: ThreadId,
    ) -> Result<()> {
        loop {
            if matches!(
                self.oracle_state.phase,
                OracleSupervisorPhase::Disabled | OracleSupervisorPhase::WaitingForOracle(_)
            ) {
                return Ok(());
            }
            let next_thread = self
                .oracle_state
                .bindings
                .get(&oracle_thread_id)
                .and_then(|binding| binding.pending_checkpoint_threads.first().copied());
            let Some(thread_id) = next_thread else {
                return Ok(());
            };
            let expected_workflow_version = self
                .oracle_state
                .bindings
                .get(&oracle_thread_id)
                .and_then(|binding| binding.pending_checkpoint_versions.get(&thread_id).copied());
            let phase_before = self.oracle_state.phase;
            let active_checkpoint_before = self.oracle_state.active_checkpoint_thread_id;
            self.handle_orchestrator_checkpoint(app_server, thread_id, expected_workflow_version)
                .await?;
            let next_after = self
                .oracle_state
                .bindings
                .get(&oracle_thread_id)
                .and_then(|binding| binding.pending_checkpoint_threads.first().copied());
            if next_after == Some(thread_id)
                && self.oracle_state.phase == phase_before
                && self.oracle_state.active_checkpoint_thread_id == active_checkpoint_before
            {
                return Ok(());
            }
        }
    }

    async fn handle_oracle_run_completed_with_remote_metadata(
        &mut self,
        app_server: &mut AppServerSession,
        result: OracleRunResult,
    ) -> Result<()> {
        let (mut conversation_id, remote_title) = take_oracle_run_remote_metadata(&result.run_id);
        if !self.oracle_state.intercepts(result.oracle_thread_id) {
            return Ok(());
        }
        self.activate_oracle_binding(result.oracle_thread_id);
        let was_aborted = self.consume_aborted_oracle_run(&result.run_id);
        let is_active = self.is_active_oracle_run(result.oracle_thread_id, &result.run_id);
        if (was_aborted || is_active)
            && conversation_id
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
        {
            let run_used_followup_session = self
                .oracle_state
                .inflight_run_requests
                .get(&result.run_id)
                .and_then(|request| request.followup_session.as_deref())
                .is_some_and(|session| !session.trim().is_empty());
            if run_used_followup_session {
                conversation_id = self
                    .oracle_state
                    .bindings
                    .get(&result.oracle_thread_id)
                    .and_then(|binding| binding.conversation_id.clone())
                    .filter(|value| !value.trim().is_empty());
            }
        }
        if let Some(message) = (was_aborted || is_active)
            .then(|| {
                self.oracle_remote_metadata_identity_error(
                    result.oracle_thread_id,
                    conversation_id.as_deref(),
                )
            })
            .flatten()
        {
            if is_active {
                self.set_oracle_thread_session_state(result.oracle_thread_id, None, None);
            }
            return self
                .handle_oracle_run_failed(
                    app_server,
                    result.run_id,
                    result.oracle_thread_id,
                    result.kind,
                    result.requested_slug,
                    message,
                )
                .await;
        }
        if (was_aborted || is_active) && (conversation_id.is_some() || remote_title.is_some()) {
            self.merge_oracle_remote_metadata(
                result.oracle_thread_id,
                conversation_id,
                remote_title,
            );
        }
        if was_aborted {
            self.persist_active_oracle_binding();
            return Ok(());
        }
        if !is_active {
            return Ok(());
        }
        self.handle_oracle_run_completed_active(app_server, result)
            .await
    }

    #[cfg_attr(not(test), allow(dead_code))]

    async fn handle_oracle_run_completed(
        &mut self,
        app_server: &mut AppServerSession,
        result: OracleRunResult,
    ) -> Result<()> {
        self.handle_oracle_run_completed_with_remote_metadata(app_server, result)
            .await
    }

    async fn handle_oracle_run_completed_active(
        &mut self,
        app_server: &mut AppServerSession,
        result: OracleRunResult,
    ) -> Result<()> {
        self.oracle_state
            .inflight_runs
            .remove(&result.oracle_thread_id);
        self.oracle_state
            .inflight_run_requests
            .remove(&result.run_id);
        self.oracle_state.active_run_id = None;
        crate::session_log::log_oracle_run_completed(&result);
        let session_slug = result.requested_slug.clone();
        self.oracle_state.session_root_slug = Some(session_slug.clone());
        self.set_oracle_thread_run_session_id(
            result.oracle_thread_id,
            Some(result.session_id.clone()),
            session_slug.clone(),
        );
        self.oracle_state.last_status = Some(match result.kind {
            OracleRequestKind::UserTurn => {
                "Oracle finished the direct supervisor turn.".to_string()
            }
            OracleRequestKind::Checkpoint => {
                "Oracle finished reviewing the orchestrator checkpoint.".to_string()
            }
        });
        let action = result.response.action;
        let directive = result.response.directive.clone();
        let delegated_task = result.response.task_for_orchestrator.clone().or_else(|| {
            directive
                .as_ref()
                .and_then(oracle_delegate_task_from_directive)
        });
        let requested_context = result.response.context_requests.clone();
        if let Some(message) = self.active_oracle_workflow_stale_reason(directive.as_ref()) {
            if matches!(result.kind, OracleRequestKind::Checkpoint) {
                self.commit_active_oracle_checkpoint(result.oracle_thread_id);
            }
            self.oracle_state.phase = OracleSupervisorPhase::Idle;
            self.oracle_state.last_status = Some(message.clone());
            if matches!(result.kind, OracleRequestKind::UserTurn) {
                self.complete_pending_oracle_turn(result.oracle_thread_id, &message)
                    .await;
            } else {
                self.append_oracle_agent_turn(result.oracle_thread_id, &message)
                    .await;
            }
            self.persist_active_oracle_binding();
            self.maybe_process_pending_oracle_checkpoints(app_server, result.oracle_thread_id)
                .await?;
            return Ok(());
        }

        let repair_message = self.oracle_control_repair_message(&result);
        let repair_exhausted = if Self::oracle_run_missing_required_control(&result) {
            Self::oracle_control_repair_exhausted(&result)
        } else {
            result.repair_attempt > 0
        };
        if repair_message.is_some() && repair_exhausted {
            let advance_pending_checkpoints = matches!(result.kind, OracleRequestKind::Checkpoint);
            if advance_pending_checkpoints {
                // A checkpoint review that already failed repair once is terminal for that
                // checkpoint. Drop it instead of requeueing the same broker turn again.
                self.commit_active_oracle_checkpoint(result.oracle_thread_id);
            }
            let message = repair_message.unwrap_or_else(|| {
                "Oracle replied without the required structured control block after a retry."
                    .to_string()
            });
            self.oracle_state.phase = OracleSupervisorPhase::Idle;
            self.oracle_state.last_status = Some(message.clone());
            self.complete_pending_oracle_turn_with_status_event(result.oracle_thread_id, &message)
                .await;
            self.chat_widget.add_error_message(message);
            self.persist_active_oracle_binding();
            if advance_pending_checkpoints {
                self.maybe_process_pending_oracle_checkpoints(app_server, result.oracle_thread_id)
                    .await?;
            }
            return Ok(());
        }
        if let Some(repair_message) = repair_message {
            let repair_context = match result.kind {
                OracleRequestKind::UserTurn => result
                    .source_user_text
                    .clone()
                    .unwrap_or_else(|| result.requested_prompt.clone()),
                OracleRequestKind::Checkpoint => result.requested_prompt.clone(),
            };
            if repair_context.trim().is_empty() {
                let advance_deferred_checkpoint =
                    if matches!(result.kind, OracleRequestKind::Checkpoint) {
                        self.defer_active_oracle_checkpoint(result.oracle_thread_id)
                    } else {
                        false
                    };
                let message =
                    "Oracle omitted required machine control and Codex could not reconstruct the original repair context."
                        .to_string();
                self.oracle_state.phase = OracleSupervisorPhase::Idle;
                self.oracle_state.last_status = Some(message.clone());
                self.complete_pending_oracle_turn_with_status_event(
                    result.oracle_thread_id,
                    &message,
                )
                .await;
                self.chat_widget.add_error_message(message);
                self.persist_active_oracle_binding();
                if advance_deferred_checkpoint {
                    self.maybe_process_pending_oracle_checkpoints(
                        app_server,
                        result.oracle_thread_id,
                    )
                    .await?;
                }
                return Ok(());
            }
            let Some(oracle_repo) = find_oracle_repo(self.chat_widget.config_ref().cwd.as_path())
            else {
                let advance_deferred_checkpoint =
                    if matches!(result.kind, OracleRequestKind::Checkpoint) {
                        self.defer_active_oracle_checkpoint(result.oracle_thread_id)
                    } else {
                        false
                    };
                let message = Self::oracle_repo_not_found_message().to_string();
                self.oracle_state.phase = OracleSupervisorPhase::Idle;
                self.oracle_state.last_status = Some(message.clone());
                self.complete_pending_oracle_turn_with_status_event(
                    result.oracle_thread_id,
                    &message,
                )
                .await;
                self.chat_widget.add_error_message(message);
                self.persist_active_oracle_binding();
                if advance_deferred_checkpoint {
                    self.maybe_process_pending_oracle_checkpoints(
                        app_server,
                        result.oracle_thread_id,
                    )
                    .await?;
                }
                return Ok(());
            };
            let message = format!(
                "Oracle produced invalid machine control; requesting a structured retry: {repair_message}"
            );
            self.oracle_state.last_status = Some(message.clone());
            self.append_oracle_status_event(result.oracle_thread_id, &message)
                .await;
            if let Err(err) = self.start_oracle_run(OracleRunRequest {
                oracle_thread_id: result.oracle_thread_id,
                kind: result.kind,
                session_slug: session_slug.clone(),
                prompt: build_control_repair_prompt(
                    &self.oracle_state,
                    result.kind,
                    &repair_context,
                    &result.response.raw_output,
                    &repair_message,
                ),
                requested_prompt: result.requested_prompt.clone(),
                source_user_text: result.source_user_text.clone(),
                files: result.files.clone(),
                workspace_cwd: self.chat_widget.config_ref().cwd.to_path_buf(),
                oracle_repo,
                followup_session: self.oracle_followup_session_for_thread(result.oracle_thread_id),
                model: self.oracle_state.model,
                browser_model_strategy: self.oracle_browser_model_strategy().to_string(),
                browser_model_label: Some(self.oracle_state.model.browser_label().to_string()),
                browser_thinking_time: self
                    .oracle_state
                    .model
                    .browser_thinking_time()
                    .map(str::to_string),
                requires_control: true,
                repair_attempt: result.repair_attempt.saturating_add(1),
                transport_retry_attempt: 0,
            }) {
                let advance_deferred_checkpoint =
                    if matches!(result.kind, OracleRequestKind::Checkpoint) {
                        self.defer_active_oracle_checkpoint(result.oracle_thread_id)
                    } else {
                        false
                    };
                let message = format!("Oracle failed before the structured retry started: {err}");
                self.oracle_state.phase = OracleSupervisorPhase::Idle;
                self.oracle_state.last_status = Some(message.clone());
                self.complete_pending_oracle_turn_with_status_event(
                    result.oracle_thread_id,
                    &message,
                )
                .await;
                self.chat_widget
                    .add_error_message(format!("Oracle supervisor failed: {err}"));
                self.persist_active_oracle_binding();
                if advance_deferred_checkpoint {
                    self.maybe_process_pending_oracle_checkpoints(
                        app_server,
                        result.oracle_thread_id,
                    )
                    .await?;
                }
                return Ok(());
            }
            self.persist_active_oracle_binding();
            return Ok(());
        }
        self.detach_terminal_oracle_workflow_for_new_user_turn_if_needed(
            action,
            directive.as_ref(),
        );

        if let Some(duplicate_message) = directive
            .as_ref()
            .and_then(|directive| self.oracle_duplicate_control_message(directive))
        {
            if matches!(result.kind, OracleRequestKind::Checkpoint) {
                self.commit_active_oracle_checkpoint(result.oracle_thread_id);
            }
            self.oracle_state.phase = if self.oracle_state.orchestrator_thread_id.is_some() {
                OracleSupervisorPhase::WaitingForOrchestrator
            } else {
                OracleSupervisorPhase::Idle
            };
            self.oracle_state.last_status = Some(duplicate_message.clone());
            if matches!(result.kind, OracleRequestKind::UserTurn) {
                self.complete_pending_oracle_turn_without_reply(result.oracle_thread_id)
                    .await;
            }
            self.append_oracle_status_event(result.oracle_thread_id, &duplicate_message)
                .await;
            self.persist_active_oracle_binding();
            self.maybe_process_pending_oracle_checkpoints(app_server, result.oracle_thread_id)
                .await?;
            return Ok(());
        }

        let mut message_for_user = result.response.message_for_user.trim().to_string();
        if message_for_user.is_empty() && matches!(action, OracleAction::Delegate) {
            message_for_user = "Oracle delegated work to the orchestrator.".to_string();
        }
        if message_for_user.is_empty()
            && matches!(action, OracleAction::RequestContext)
            && !requested_context.is_empty()
        {
            message_for_user = format!(
                "Oracle requested more context: {}",
                requested_context.join(", ")
            );
        }
        match action {
            OracleAction::Delegate => {
                let Some(task) = delegated_task else {
                    let message =
                        "Oracle requested a workflow handoff without an orchestrator task."
                            .to_string();
                    self.oracle_state.phase = OracleSupervisorPhase::Idle;
                    self.oracle_state.last_status = Some(message.clone());
                    self.record_oracle_status_event_for_run_kind(
                        result.oracle_thread_id,
                        result.kind,
                        &message,
                    )
                    .await;
                    self.chat_widget.add_error_message(message);
                    self.persist_active_oracle_binding();
                    return Ok(());
                };
                self.adopt_replacement_oracle_workflow_if_allowed(
                    result.kind,
                    OracleAction::Delegate,
                    directive.as_ref(),
                );
                let workflow = self.ensure_active_oracle_workflow(
                    directive
                        .as_ref()
                        .and_then(|value| value.workflow_id.clone()),
                    directive
                        .as_ref()
                        .and_then(|value| value.objective.clone())
                        .or_else(|| Some(task.clone())),
                    directive
                        .as_ref()
                        .and_then(|value| value.summary.clone())
                        .or_else(|| {
                            (!message_for_user.is_empty()).then_some(message_for_user.clone())
                        }),
                    directive.as_ref().and_then(|value| value.workflow_version),
                );
                self.oracle_state.accept_user_turn_workflow_replacement = false;
                let thread_id = match self.ensure_orchestrator_thread(app_server).await {
                    Ok(thread_id) => thread_id,
                    Err(err) => {
                        if matches!(result.kind, OracleRequestKind::Checkpoint) {
                            self.commit_active_oracle_checkpoint(result.oracle_thread_id);
                        }
                        let message = format!(
                            "Oracle delegated a task, but Codex failed before the orchestrator thread was ready: {err}"
                        );
                        self.oracle_state.phase = OracleSupervisorPhase::Idle;
                        self.oracle_state.last_status = Some(message.clone());
                        self.record_oracle_status_event_for_run_kind(
                            result.oracle_thread_id,
                            result.kind,
                            &message,
                        )
                        .await;
                        self.chat_widget.add_error_message(message);
                        self.persist_active_oracle_binding();
                        if matches!(result.kind, OracleRequestKind::Checkpoint) {
                            self.maybe_process_pending_oracle_checkpoints(
                                app_server,
                                result.oracle_thread_id,
                            )
                            .await?;
                        }
                        return Ok(());
                    }
                };
                if let Some(active_workflow) = self.oracle_state.workflow.as_mut() {
                    active_workflow.status = Self::oracle_workflow_status_for_action(
                        OracleAction::Delegate,
                        directive.as_ref().and_then(|value| value.status.as_deref()),
                        OracleWorkflowStatus::Running,
                    );
                    active_workflow.orchestrator_thread_id = Some(thread_id);
                    if let Some(summary) =
                        directive.as_ref().and_then(|value| value.summary.clone())
                    {
                        active_workflow.summary = Some(summary);
                    }
                    active_workflow.last_blocker = None;
                }
                self.oracle_state.orchestrator_thread_id = Some(thread_id);
                let op = self
                    .oracle_destination_task_op(
                        result.oracle_thread_id,
                        thread_id,
                        ORACLE_ORCHESTRATOR_DESTINATION,
                        Some("orchestrator"),
                        task.clone(),
                        Some(orchestrator_developer_instructions()),
                    )
                    .await;
                self.oracle_state.last_orchestrator_task = Some(task);
                self.oracle_state.last_status = Some(format!(
                    "Oracle handed workflow `{}` v{} to orchestrator thread {thread_id}.",
                    workflow.workflow_id, workflow.version
                ));
                self.oracle_state.phase = OracleSupervisorPhase::WaitingForOrchestrator;
                self.oracle_state.automatic_context_followups = 0;
                if let Some(directive) = directive.as_ref() {
                    self.reserve_pending_oracle_control(directive);
                    self.persist_active_oracle_binding();
                }
                if let Err(err) = self.submit_thread_op(app_server, thread_id, op).await {
                    if let Some(directive) = directive.as_ref() {
                        self.release_pending_oracle_control(directive);
                    }
                    if matches!(result.kind, OracleRequestKind::Checkpoint) {
                        self.commit_active_oracle_checkpoint(result.oracle_thread_id);
                    }
                    self.oracle_state.phase = OracleSupervisorPhase::Idle;
                    self.update_active_oracle_workflow_status(
                        OracleWorkflowStatus::Failed,
                        directive.as_ref().and_then(|value| value.summary.clone()),
                        Some(err.to_string()),
                    );
                    let message = format!(
                        "Oracle delegated a task, but Codex failed to start the orchestrator turn: {err}"
                    );
                    self.oracle_state.last_status = Some(message.clone());
                    self.record_oracle_status_event_for_run_kind(
                        result.oracle_thread_id,
                        result.kind,
                        &message,
                    )
                    .await;
                    self.chat_widget.add_error_message(message);
                    self.persist_active_oracle_binding();
                    if matches!(result.kind, OracleRequestKind::Checkpoint) {
                        self.maybe_process_pending_oracle_checkpoints(
                            app_server,
                            result.oracle_thread_id,
                        )
                        .await?;
                    }
                    return Ok(());
                }
                let delegate_message = Self::oracle_delegate_user_message(&message_for_user);
                if matches!(result.kind, OracleRequestKind::UserTurn) {
                    self.complete_pending_oracle_turn(result.oracle_thread_id, &delegate_message)
                        .await;
                } else {
                    self.append_oracle_agent_turn(result.oracle_thread_id, &delegate_message)
                        .await;
                }
                self.append_oracle_workflow_event(
                    result.oracle_thread_id,
                    Self::oracle_delegate_thread_event(&workflow, &message_for_user),
                )
                .await;
                if let Some(directive) = directive.as_ref() {
                    self.record_applied_oracle_control(directive);
                }
            }
            OracleAction::RequestContext => {
                let requests = requested_context;
                if requests.is_empty() {
                    let message =
                        "Oracle requested more context without any context_requests.".to_string();
                    if matches!(result.kind, OracleRequestKind::Checkpoint) {
                        self.commit_active_oracle_checkpoint(result.oracle_thread_id);
                    }
                    self.oracle_state.phase = OracleSupervisorPhase::Idle;
                    self.oracle_state.last_status = Some(message.clone());
                    if matches!(result.kind, OracleRequestKind::UserTurn) {
                        self.complete_pending_oracle_turn(result.oracle_thread_id, &message)
                            .await;
                    } else {
                        self.append_oracle_agent_turn(result.oracle_thread_id, &message)
                            .await;
                    }
                    self.persist_active_oracle_binding();
                    if matches!(result.kind, OracleRequestKind::Checkpoint) {
                        self.maybe_process_pending_oracle_checkpoints(
                            app_server,
                            result.oracle_thread_id,
                        )
                        .await?;
                    }
                    return Ok(());
                }
                self.adopt_replacement_oracle_workflow_if_allowed(
                    result.kind,
                    OracleAction::RequestContext,
                    directive.as_ref(),
                );
                if self.oracle_state.automatic_context_followups >= 1 {
                    let message = format!(
                        "Oracle requested more context again after the automatic follow-up. Review the requested context manually: {}",
                        requests.join(", ")
                    );
                    if matches!(result.kind, OracleRequestKind::Checkpoint) {
                        self.commit_active_oracle_checkpoint(result.oracle_thread_id);
                    }
                    self.oracle_state.phase = OracleSupervisorPhase::Idle;
                    self.oracle_state.last_status = Some(message.clone());
                    if matches!(result.kind, OracleRequestKind::UserTurn) {
                        self.complete_pending_oracle_turn(result.oracle_thread_id, &message)
                            .await;
                    } else {
                        self.append_oracle_agent_turn(result.oracle_thread_id, &message)
                            .await;
                    }
                    self.persist_active_oracle_binding();
                    if matches!(result.kind, OracleRequestKind::Checkpoint) {
                        self.maybe_process_pending_oracle_checkpoints(
                            app_server,
                            result.oracle_thread_id,
                        )
                        .await?;
                    }
                    return Ok(());
                }
                let Some(oracle_repo) =
                    find_oracle_repo(self.chat_widget.config_ref().cwd.as_path())
                else {
                    let message = Self::oracle_repo_not_found_message().to_string();
                    if matches!(result.kind, OracleRequestKind::Checkpoint) {
                        self.commit_active_oracle_checkpoint(result.oracle_thread_id);
                    }
                    self.oracle_state.phase = OracleSupervisorPhase::Idle;
                    self.oracle_state.last_status = Some(message.clone());
                    if matches!(result.kind, OracleRequestKind::UserTurn) {
                        self.complete_pending_oracle_turn(result.oracle_thread_id, &message)
                            .await;
                    } else {
                        self.append_oracle_agent_turn(result.oracle_thread_id, &message)
                            .await;
                    }
                    self.chat_widget.add_error_message(message);
                    self.persist_active_oracle_binding();
                    if matches!(result.kind, OracleRequestKind::Checkpoint) {
                        self.maybe_process_pending_oracle_checkpoints(
                            app_server,
                            result.oracle_thread_id,
                        )
                        .await?;
                    }
                    return Ok(());
                };
                let orchestrator = if let Some(thread_id) = self.oracle_state.orchestrator_thread_id
                {
                    match app_server.read_thread_with_turns(thread_id).await {
                        Ok(thread) => Some(thread),
                        Err(err) => {
                            let message = format!(
                                "Oracle requested more context, but Codex could not read orchestrator thread {thread_id}: {err}"
                            );
                            if matches!(result.kind, OracleRequestKind::Checkpoint) {
                                self.commit_active_oracle_checkpoint(result.oracle_thread_id);
                            }
                            self.oracle_state.phase = OracleSupervisorPhase::Idle;
                            self.oracle_state.last_status = Some(message.clone());
                            self.record_oracle_status_event_for_run_kind(
                                result.oracle_thread_id,
                                result.kind,
                                &message,
                            )
                            .await;
                            self.chat_widget.add_error_message(message);
                            self.persist_active_oracle_binding();
                            if matches!(result.kind, OracleRequestKind::Checkpoint) {
                                self.maybe_process_pending_oracle_checkpoints(
                                    app_server,
                                    result.oracle_thread_id,
                                )
                                .await?;
                            }
                            return Ok(());
                        }
                    }
                } else {
                    None
                };
                let cwd = orchestrator
                    .as_ref()
                    .map(|thread| thread.cwd.clone())
                    .unwrap_or_else(|| self.chat_widget.config_ref().cwd.clone());
                let orchestrator_summary = orchestrator.as_ref().map(summarize_thread);
                let context = resolve_context_requests(
                    cwd.as_path(),
                    &requests,
                    orchestrator_summary.as_deref(),
                )
                .await;
                if let Some(workflow) = self.oracle_state.workflow.as_mut() {
                    workflow.status = Self::oracle_workflow_status_for_action(
                        OracleAction::RequestContext,
                        directive.as_ref().and_then(|value| value.status.as_deref()),
                        OracleWorkflowStatus::Running,
                    );
                    if let Some(summary) =
                        directive.as_ref().and_then(|value| value.summary.clone())
                    {
                        workflow.summary = Some(summary);
                    }
                }
                self.oracle_state.automatic_context_followups += 1;
                self.oracle_state.last_status = Some(format!(
                    "Oracle requested more context: {}",
                    requests.join(", ")
                ));
                self.append_oracle_workflow_event(
                    result.oracle_thread_id,
                    Self::oracle_context_request_thread_event(&requests, &message_for_user),
                )
                .await;
                let context_prompt = build_context_prompt(&self.oracle_state, &requests, &context);
                if let Some(directive) = directive.as_ref() {
                    self.reserve_pending_oracle_control(directive);
                    self.persist_active_oracle_binding();
                }
                if let Err(err) = self.start_oracle_run(OracleRunRequest {
                    oracle_thread_id: result.oracle_thread_id,
                    kind: result.kind,
                    session_slug,
                    prompt: context_prompt.clone(),
                    requested_prompt: context_prompt,
                    source_user_text: None,
                    files: Vec::new(),
                    workspace_cwd: cwd.to_path_buf(),
                    oracle_repo,
                    followup_session: self
                        .oracle_followup_session_for_thread(result.oracle_thread_id),
                    model: self.oracle_state.model,
                    browser_model_strategy: self.oracle_browser_model_strategy().to_string(),
                    browser_model_label: Some(self.oracle_state.model.browser_label().to_string()),
                    browser_thinking_time: self
                        .oracle_state
                        .model
                        .browser_thinking_time()
                        .map(str::to_string),
                    requires_control: true,
                    repair_attempt: 0,
                    transport_retry_attempt: 0,
                }) {
                    if let Some(directive) = directive.as_ref() {
                        self.release_pending_oracle_control(directive);
                    }
                    let message =
                        format!("Oracle failed to continue after requesting context: {err}");
                    if matches!(result.kind, OracleRequestKind::Checkpoint) {
                        self.commit_active_oracle_checkpoint(result.oracle_thread_id);
                    }
                    self.oracle_state.phase = OracleSupervisorPhase::Idle;
                    self.oracle_state.last_status = Some(message.clone());
                    if matches!(result.kind, OracleRequestKind::UserTurn) {
                        self.complete_pending_oracle_turn(result.oracle_thread_id, &message)
                            .await;
                    } else {
                        self.append_oracle_agent_turn(result.oracle_thread_id, &message)
                            .await;
                    }
                    self.chat_widget
                        .add_error_message(format!("Oracle supervisor failed: {err}"));
                    self.persist_active_oracle_binding();
                    if matches!(result.kind, OracleRequestKind::Checkpoint) {
                        self.maybe_process_pending_oracle_checkpoints(
                            app_server,
                            result.oracle_thread_id,
                        )
                        .await?;
                    }
                    return Ok(());
                }
                if let Some(directive) = directive.as_ref() {
                    self.record_applied_oracle_control(directive);
                }
            }
            OracleAction::Reply
            | OracleAction::AskUser
            | OracleAction::Checkpoint
            | OracleAction::Finish => {
                if !matches!(action, OracleAction::Reply) {
                    self.adopt_replacement_oracle_workflow_if_allowed(
                        result.kind,
                        action,
                        directive.as_ref(),
                    );
                }
                let finish_status = if matches!(action, OracleAction::Finish) {
                    Some(Self::oracle_workflow_status_for_action(
                        OracleAction::Finish,
                        directive.as_ref().and_then(|value| value.status.as_deref()),
                        OracleWorkflowStatus::Complete,
                    ))
                } else {
                    None
                };
                let final_message = if message_for_user.is_empty() {
                    match result.response.action {
                        OracleAction::AskUser => "Oracle is waiting on the human.".to_string(),
                        OracleAction::Checkpoint => {
                            "Oracle surfaced a workflow checkpoint.".to_string()
                        }
                        OracleAction::Finish => {
                            if finish_status == Some(OracleWorkflowStatus::Failed) {
                                "Oracle marked the workflow failed.".to_string()
                            } else {
                                "Oracle marked the milestone complete.".to_string()
                            }
                        }
                        _ => "Oracle replied.".to_string(),
                    }
                } else {
                    message_for_user
                };
                if matches!(result.kind, OracleRequestKind::UserTurn) {
                    self.complete_pending_oracle_turn(result.oracle_thread_id, &final_message)
                        .await;
                } else {
                    self.append_oracle_agent_turn(result.oracle_thread_id, &final_message)
                        .await;
                }
                self.oracle_state.phase = OracleSupervisorPhase::Idle;
                self.oracle_state.automatic_context_followups = 0;
                match action {
                    OracleAction::AskUser => {
                        self.update_active_oracle_workflow_status(
                            Self::oracle_workflow_status_for_action(
                                OracleAction::AskUser,
                                directive.as_ref().and_then(|value| value.status.as_deref()),
                                OracleWorkflowStatus::NeedsHuman,
                            ),
                            directive
                                .as_ref()
                                .and_then(|value| value.summary.clone())
                                .or_else(|| Some(final_message.clone())),
                            Some(final_message.clone()),
                        );
                    }
                    OracleAction::Checkpoint => {
                        self.update_active_oracle_workflow_status(
                            Self::oracle_workflow_status_for_action(
                                OracleAction::Checkpoint,
                                directive.as_ref().and_then(|value| value.status.as_deref()),
                                OracleWorkflowStatus::Running,
                            ),
                            directive
                                .as_ref()
                                .and_then(|value| value.summary.clone())
                                .or_else(|| Some(final_message.clone())),
                            None,
                        );
                    }
                    OracleAction::Finish => {
                        self.clear_oracle_orchestrator_binding(result.oracle_thread_id);
                        self.update_active_oracle_workflow_status(
                            finish_status.unwrap_or(OracleWorkflowStatus::Complete),
                            directive
                                .as_ref()
                                .and_then(|value| value.summary.clone())
                                .or_else(|| Some(final_message.clone())),
                            None,
                        );
                    }
                    OracleAction::Reply => {
                        let reply_status = Self::oracle_workflow_status_for_action(
                            OracleAction::Reply,
                            directive.as_ref().and_then(|value| value.status.as_deref()),
                            OracleWorkflowStatus::NeedsHuman,
                        );
                        let reply_blocker = matches!(
                            reply_status,
                            OracleWorkflowStatus::NeedsHuman | OracleWorkflowStatus::Failed
                        )
                        .then(|| final_message.clone())
                        .filter(|value| !value.trim().is_empty());
                        self.update_active_oracle_workflow_status(
                            reply_status,
                            directive
                                .as_ref()
                                .and_then(|value| value.summary.clone())
                                .or_else(|| Some(final_message.clone())),
                            reply_blocker,
                        );
                    }
                    _ => {}
                }
                self.oracle_state.last_status = Some(match action {
                    OracleAction::AskUser => "Oracle is waiting on the human.".to_string(),
                    OracleAction::Checkpoint => {
                        "Oracle surfaced a non-terminal workflow checkpoint.".to_string()
                    }
                    OracleAction::Finish => {
                        if finish_status == Some(OracleWorkflowStatus::Failed) {
                            "Oracle marked the workflow failed.".to_string()
                        } else {
                            "Oracle marked the milestone complete.".to_string()
                        }
                    }
                    _ => "Oracle replied.".to_string(),
                });
                if let Some(directive) = directive.as_ref() {
                    self.record_applied_oracle_control(directive);
                }
            }
        }
        if matches!(result.kind, OracleRequestKind::Checkpoint)
            && !matches!(action, OracleAction::RequestContext)
            && self.oracle_state.phase
                != OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint)
        {
            self.commit_active_oracle_checkpoint(result.oracle_thread_id);
        }
        self.shutdown_oracle_broker_gracefully().await;
        self.persist_active_oracle_binding();
        self.maybe_process_pending_oracle_checkpoints(app_server, result.oracle_thread_id)
            .await?;
        Ok(())
    }

    async fn handle_oracle_run_failed(
        &mut self,
        app_server: &mut AppServerSession,
        run_id: String,
        visible_thread_id: ThreadId,
        kind: OracleRequestKind,
        session_slug: String,
        error: String,
    ) -> Result<()> {
        if !self.oracle_state.intercepts(visible_thread_id) {
            return Ok(());
        }
        self.activate_oracle_binding(visible_thread_id);
        if self.consume_aborted_oracle_run(&run_id) {
            self.persist_active_oracle_binding();
            return Ok(());
        }
        if !self.is_active_oracle_run(visible_thread_id, &run_id) {
            return Ok(());
        }
        self.oracle_state.inflight_runs.remove(&visible_thread_id);
        crate::session_log::log_oracle_run_failed(
            &run_id,
            visible_thread_id,
            kind,
            &session_slug,
            &error,
        );
        let failed_request = self.oracle_state.inflight_run_requests.remove(&run_id);
        self.oracle_state.active_run_id = None;
        let error = format!("{error} (requested slug: {session_slug}, phase: {kind:?})");
        let allow_workflow_replacement = self.oracle_state.accept_user_turn_workflow_replacement;
        if let Some(mut retry_request) = failed_request
            .filter(|request| Self::oracle_failure_supports_direct_retry(request, &error))
        {
            if Self::oracle_failure_requires_broker_reset(&error) {
                self.shutdown_oracle_broker_gracefully().await;
            }
            retry_request.followup_session = self
                .oracle_followup_session_for_thread(visible_thread_id)
                .or(retry_request.followup_session.clone());
            retry_request.transport_retry_attempt =
                retry_request.transport_retry_attempt.saturating_add(1);
            let message = format!(
                "Oracle broker transport died before replying; retrying the direct Oracle turn once. Cause: {}",
                Self::oracle_transport_retry_error_excerpt(&error)
            );
            self.oracle_state.last_status = Some(message.clone());
            self.append_oracle_status_event(visible_thread_id, &message)
                .await;
            self.oracle_state.accept_user_turn_workflow_replacement = allow_workflow_replacement;
            if let Err(retry_error) = self.start_oracle_run(retry_request) {
                let retry_failure = format!(
                    "Oracle failed: {error}. Retrying the direct Oracle turn also failed to start: {retry_error}"
                );
                self.oracle_state.phase = OracleSupervisorPhase::Idle;
                self.oracle_state.last_status = Some(retry_failure.clone());
                self.record_oracle_status_event_for_run_kind(
                    visible_thread_id,
                    kind,
                    &retry_failure,
                )
                .await;
                self.chat_widget.add_error_message(retry_failure);
                self.persist_active_oracle_binding();
                self.maybe_process_pending_oracle_checkpoints(app_server, visible_thread_id)
                    .await?;
            }
            return Ok(());
        }
        self.oracle_state.accept_user_turn_workflow_replacement = false;
        let message = format!("Oracle failed: {error}");
        self.oracle_state.phase = OracleSupervisorPhase::Idle;
        self.oracle_state.last_status = Some(message.clone());
        if matches!(kind, OracleRequestKind::Checkpoint) {
            self.requeue_active_oracle_checkpoint(visible_thread_id);
        }
        if Self::oracle_failure_requires_broker_reset(&error) {
            self.shutdown_oracle_broker_gracefully().await;
        }
        if matches!(kind, OracleRequestKind::UserTurn) {
            self.complete_pending_oracle_turn(visible_thread_id, &message)
                .await;
        } else {
            self.append_oracle_agent_turn(visible_thread_id, &message)
                .await;
        }
        self.chat_widget.add_error_message(message);
        self.shutdown_oracle_broker_gracefully().await;
        self.persist_active_oracle_binding();
        self.maybe_process_pending_oracle_checkpoints(app_server, visible_thread_id)
            .await?;
        Ok(())
    }

    fn oracle_failure_requires_broker_reset(error: &str) -> bool {
        let normalized = error.to_ascii_lowercase();
        normalized.contains("oracle broker")
            || normalized.contains("failed to parse oracle broker")
            || normalized.contains("failed to serialize oracle broker")
            || normalized.contains("exited without replying")
            || normalized.contains("oracle broker stdin")
            || normalized.contains("oracle broker stdout")
    }

    fn oracle_failure_supports_direct_retry(request: &OracleRunRequest, error: &str) -> bool {
        if !matches!(request.kind, OracleRequestKind::UserTurn)
            || request.requires_control
            || !request.files.is_empty()
            || request.transport_retry_attempt > 0
        {
            return false;
        }
        let normalized = error.to_ascii_lowercase();
        normalized.contains("oracle broker")
            || normalized.contains("closed before replying")
            || normalized.contains("exited without replying")
    }

    fn oracle_transport_retry_error_excerpt(error: &str) -> String {
        let excerpt = error
            .split(" (requested slug:")
            .next()
            .unwrap_or(error)
            .lines()
            .next()
            .unwrap_or(error)
            .trim();
        const MAX_LEN: usize = 180;
        if excerpt.chars().count() <= MAX_LEN {
            excerpt.to_string()
        } else {
            let truncated = excerpt.chars().take(MAX_LEN - 1).collect::<String>();
            format!("{truncated}…")
        }
    }

    async fn handle_orchestrator_checkpoint(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        expected_workflow_version: Option<u64>,
    ) -> Result<()> {
        let Some(oracle_thread_id) = self.find_oracle_thread_by_orchestrator_thread(thread_id)
        else {
            return Ok(());
        };
        self.activate_oracle_binding(oracle_thread_id);
        if matches!(
            self.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOracle(_)
        ) {
            self.persist_active_oracle_binding();
            return Ok(());
        }
        if !self
            .oracle_state
            .workflow
            .as_ref()
            .is_some_and(|workflow| matches!(workflow.status, OracleWorkflowStatus::Running))
        {
            tracing::info!(
                %thread_id,
                workflow_status = self
                    .oracle_state
                    .workflow
                    .as_ref()
                    .map(|workflow| workflow.status.label()),
                "ignoring orchestrator checkpoint because the workflow is no longer running"
            );
            self.clear_pending_oracle_checkpoint(oracle_thread_id, thread_id);
            self.emit_next_pending_oracle_checkpoint(oracle_thread_id);
            self.persist_active_oracle_binding();
            return Ok(());
        }
        if let Some(expected_workflow_version) = expected_workflow_version
            && self
                .oracle_state
                .workflow
                .as_ref()
                .is_some_and(|workflow| workflow.version != expected_workflow_version)
        {
            tracing::info!(
                %thread_id,
                expected_workflow_version,
                current_workflow_version = self.oracle_state.workflow.as_ref().map(|workflow| workflow.version),
                "ignoring stale orchestrator checkpoint for superseded workflow version"
            );
            self.clear_pending_oracle_checkpoint(oracle_thread_id, thread_id);
            self.emit_next_pending_oracle_checkpoint(oracle_thread_id);
            self.persist_active_oracle_binding();
            return Ok(());
        }
        let Some(oracle_repo) = find_oracle_repo(self.chat_widget.config_ref().cwd.as_path())
        else {
            let message = Self::oracle_repo_not_found_message().to_string();
            self.oracle_state.phase = OracleSupervisorPhase::Idle;
            self.oracle_state.last_status = Some(message.clone());
            self.clear_pending_oracle_checkpoint(oracle_thread_id, thread_id);
            self.append_oracle_status_event(oracle_thread_id, &message)
                .await;
            self.chat_widget.add_error_message(message);
            self.persist_active_oracle_binding();
            return Ok(());
        };
        let thread = match app_server.read_thread_with_turns(thread_id).await {
            Ok(thread) => thread,
            Err(err) if Self::can_fallback_from_include_turns_error(&err) => {
                if !self.thread_event_channels.contains_key(&thread_id)
                    && !self.oracle_checkpoint_turn_is_known(oracle_thread_id, thread_id)
                {
                    tracing::info!(
                        %thread_id,
                        "failing closed orchestrator checkpoint because the thread closed before any terminal turn was materialized"
                    );
                    self.fail_closed_orchestrator_checkpoint_without_turn(
                        oracle_thread_id,
                        thread_id,
                    )
                    .await;
                    self.emit_next_pending_oracle_checkpoint(oracle_thread_id);
                    return Ok(());
                }
                tracing::info!(
                    %thread_id,
                    "deferring orchestrator checkpoint until thread turns are materialized"
                );
                self.schedule_orchestrator_checkpoint_retry(thread_id, expected_workflow_version);
                self.persist_active_oracle_binding();
                return Ok(());
            }
            Err(err) if Self::is_terminal_thread_read_error(&err) => {
                tracing::info!(
                    %thread_id,
                    error = %err,
                    "failing closed orchestrator checkpoint because the thread is no longer loaded"
                );
                self.mark_agent_picker_thread_closed(thread_id);
                self.thread_event_channels.remove(&thread_id);
                self.fail_closed_orchestrator_checkpoint_without_turn(oracle_thread_id, thread_id)
                    .await;
                self.emit_next_pending_oracle_checkpoint(oracle_thread_id);
                return Ok(());
            }
            Err(err) => return Err(err),
        };
        let binding = self.oracle_state.bindings.get(&oracle_thread_id);
        let Some(latest_turn_id) = Self::next_orchestrator_checkpoint_turn_id(
            self.oracle_state.bindings.get(&oracle_thread_id),
            thread_id,
            &thread,
        ) else {
            if let Some((pending_turn_id, visible_terminal_turn_id)) =
                Self::orchestrator_checkpoint_retry_pending_turn(binding, thread_id, &thread)
            {
                tracing::info!(
                    %thread_id,
                    %pending_turn_id,
                    %visible_terminal_turn_id,
                    "deferring orchestrator checkpoint until the newer notified terminal turn is visible"
                );
                self.schedule_orchestrator_checkpoint_retry(thread_id, expected_workflow_version);
                self.persist_active_oracle_binding();
                return Ok(());
            }
            let duplicate_turn_id = self
                .oracle_state
                .bindings
                .get(&oracle_thread_id)
                .and_then(|binding| binding.last_checkpoint_turn_ids.get(&thread_id))
                .cloned();
            if let Some(turn_id) = duplicate_turn_id {
                tracing::info!(
                    %thread_id,
                    latest_turn_id = %turn_id,
                    "ignoring duplicate orchestrator checkpoint for an already-reviewed turn"
                );
            } else {
                tracing::info!(
                    %thread_id,
                    "ignoring orchestrator checkpoint because the thread has no terminal turn yet"
                );
                if !self.thread_event_channels.contains_key(&thread_id)
                    && !self.oracle_checkpoint_turn_is_known(oracle_thread_id, thread_id)
                {
                    tracing::info!(
                        %thread_id,
                        "failing closed orchestrator checkpoint because the closed thread never exposed a terminal turn"
                    );
                    self.fail_closed_orchestrator_checkpoint_without_turn(
                        oracle_thread_id,
                        thread_id,
                    )
                    .await;
                    self.emit_next_pending_oracle_checkpoint(oracle_thread_id);
                    return Ok(());
                }
                self.schedule_orchestrator_checkpoint_retry(thread_id, expected_workflow_version);
            }
            self.persist_active_oracle_binding();
            return Ok(());
        };
        let (session_slug, followup_session) = match self
            .ensure_oracle_session_continuity_for_run(oracle_thread_id)
            .await
        {
            Ok(continuity) => continuity,
            Err(err) => {
                let message = format!(
                    "Oracle could not reattach the hidden browser thread for checkpoint review: {err}"
                );
                self.oracle_state.phase = OracleSupervisorPhase::Idle;
                self.oracle_state.last_status = Some(message.clone());
                self.append_oracle_status_event(oracle_thread_id, &message)
                    .await;
                self.schedule_orchestrator_checkpoint_retry(thread_id, expected_workflow_version);
                self.persist_active_oracle_binding();
                return Ok(());
            }
        };
        let git_status = Self::run_git_capture(thread.cwd.as_path(), &["status", "--short"]).await;
        let diff_stat =
            Self::run_git_capture(thread.cwd.as_path(), &["diff", "--stat", "--no-ext-diff"]).await;
        let checkpoint_summary = summarize_thread(&thread);
        if let Some(workflow) = self.oracle_state.workflow.as_mut() {
            workflow.status = OracleWorkflowStatus::Running;
            workflow.last_checkpoint = Some(checkpoint_summary.clone());
            workflow.summary = Some(checkpoint_summary.clone());
            workflow.orchestrator_thread_id = Some(thread_id);
        }
        self.oracle_state.last_status = Some(format!(
            "Sending orchestrator checkpoint from thread {thread_id} to Oracle."
        ));
        let checkpoint_prompt =
            build_checkpoint_prompt(&self.oracle_state, &thread, &git_status, &diff_stat);
        if let Err(err) = self.start_oracle_run(OracleRunRequest {
            oracle_thread_id,
            kind: OracleRequestKind::Checkpoint,
            session_slug,
            prompt: checkpoint_prompt.clone(),
            requested_prompt: checkpoint_prompt,
            source_user_text: None,
            files: Vec::new(),
            workspace_cwd: thread.cwd.to_path_buf(),
            oracle_repo,
            followup_session,
            model: self.oracle_state.model,
            browser_model_strategy: self.oracle_browser_model_strategy().to_string(),
            browser_model_label: Some(self.oracle_state.model.browser_label().to_string()),
            browser_thinking_time: self
                .oracle_state
                .model
                .browser_thinking_time()
                .map(str::to_string),
            requires_control: true,
            repair_attempt: 0,
            transport_retry_attempt: 0,
        }) {
            let message = format!("Oracle checkpoint failed before the run started: {err}");
            self.oracle_state.phase = OracleSupervisorPhase::Idle;
            self.oracle_state.last_status = Some(message.clone());
            self.requeue_orchestrator_checkpoint_launch_failure(
                oracle_thread_id,
                thread_id,
                expected_workflow_version,
                latest_turn_id,
            );
            self.append_oracle_status_event(oracle_thread_id, &message)
                .await;
            self.chat_widget
                .add_error_message(format!("Oracle supervisor failed: {err}"));
        } else {
            self.clear_pending_oracle_checkpoint(oracle_thread_id, thread_id);
            self.mark_active_oracle_checkpoint(
                oracle_thread_id,
                thread_id,
                latest_turn_id.to_string(),
            );
        }
        self.persist_active_oracle_binding();
        Ok(())
    }

    async fn run_git_capture(cwd: &Path, args: &[&str]) -> String {
        match Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            Ok(output) => String::from_utf8_lossy(&output.stderr).trim().to_string(),
            Err(error) => format!("git {} failed: {error}", args.join(" ")),
        }
    }

    async fn maybe_route_natural_language_oracle_invocation(
        &mut self,
        tui: Option<&mut tui::Tui>,
        app_server: &mut AppServerSession,
        origin_thread_id: ThreadId,
        items: &[UserInput],
    ) -> Result<bool> {
        if self.oracle_state.intercepts(origin_thread_id)
            || !Self::natural_language_oracle_invocation(items)
        {
            return Ok(false);
        }
        let oracle_thread_id = self
            .ensure_oracle_entrypoint_thread(tui, app_server)
            .await?;
        self.oracle_state.last_status = Some(format!(
            "Routing this turn into Oracle thread {oracle_thread_id} via the natural-language Oracle entrypoint."
        ));
        self.handle_oracle_user_turn(app_server, oracle_thread_id, items)
            .await
    }

    async fn handle_configure_oracle_command(
        &mut self,
        tui: Option<&mut tui::Tui>,
        app_server: &mut AppServerSession,
        raw_command: &str,
    ) -> Result<()> {
        let mut tui = tui;
        match OracleCommand::parse(raw_command) {
            Ok(OracleCommand::Browse) => {
                if let Err(err) = self.open_oracle_picker().await {
                    self.chat_widget
                        .add_error_message(format!("Failed to open Oracle picker: {err}"));
                }
            }
            Ok(OracleCommand::NewThread) => match self.create_oracle_thread_binding().await {
                Ok(binding) => {
                    if self.active_thread_id != Some(binding.thread_id)
                        && let Some(tui) = tui.as_deref_mut()
                    {
                        self.select_agent_thread(tui, app_server, binding.thread_id)
                            .await?;
                    }
                    self.chat_widget.add_to_history(oracle_history_cell(
                        "Oracle",
                        &Self::oracle_new_thread_history_message(&binding),
                    ));
                }
                Err(err)
                    if err.to_string() == self.oracle_remote_thread_mutation_blocked_message() =>
                {
                    self.chat_widget.add_error_message(err.to_string());
                }
                Err(err) => self
                    .chat_widget
                    .add_error_message(format!("Failed to create a new Oracle thread: {err}")),
            },
            Ok(OracleCommand::AttachThread {
                conversation_id,
                conversation_url,
                import_history,
            }) => {
                match self
                    .attach_oracle_thread_binding(
                        conversation_id.clone(),
                        conversation_url.clone(),
                        /*title_hint*/ None,
                    )
                    .await
                {
                    Ok(binding) => {
                        let thread_id = binding.thread_id();
                        if self.active_thread_id != Some(thread_id)
                            && let Some(tui) = tui.as_deref_mut()
                        {
                            self.select_agent_thread(tui, app_server, thread_id).await?;
                        }
                        if import_history {
                            let history_response = match self
                                .oracle_fetch_remote_thread_history(thread_id)
                                .await
                            {
                                Ok(history) => Some(history),
                                Err(err) => {
                                    self.chat_widget.add_error_message(format!(
                                            "Attached Oracle thread on {thread_id}, but failed to fetch remote history: {err}"
                                        ));
                                    None
                                }
                            };
                            if let Some(history_response) = history_response {
                                match self
                                    .import_oracle_thread_history(
                                        tui.as_deref_mut(),
                                        thread_id,
                                        conversation_id.as_str(),
                                        history_response.history.clone(),
                                    )
                                    .await
                                {
                                    Ok(outcome) => {
                                        let (outcome_label, message_count) = match outcome {
                                            OracleHistoryImportOutcome::Imported {
                                                message_count,
                                            } => ("imported", Some(message_count)),
                                            OracleHistoryImportOutcome::NoNewMessages => {
                                                ("no_new_messages", None)
                                            }
                                            OracleHistoryImportOutcome::SkippedToAvoidConflict => {
                                                ("skipped_to_avoid_conflict", None)
                                            }
                                        };
                                        crate::session_log::log_oracle_history_import(
                                            thread_id,
                                            conversation_id.as_str(),
                                            outcome_label,
                                            message_count,
                                            history_response.history_window.as_ref(),
                                        );
                                        let status = Self::oracle_attach_history_message(
                                            &binding,
                                            outcome,
                                            history_response.history_window.as_ref(),
                                        );
                                        self.oracle_state.last_status = Some(status);
                                        self.persist_active_oracle_binding();
                                    }
                                    Err(err) => {
                                        self.chat_widget.add_error_message(format!(
                                            "Attached Oracle thread on {thread_id}, but failed to import remote history: {err}"
                                        ));
                                    }
                                }
                            }
                        }
                        let message = self.oracle_state.last_status.clone().unwrap_or_else(|| {
                            match binding {
                                OracleAttachThreadBinding::ReattachedLocal { title, .. } => {
                                    format!(
                                        "Reattached existing local Oracle thread {thread_id}: {title}"
                                    )
                                }
                                OracleAttachThreadBinding::AttachedRemote {
                                    title,
                                    conversation_id,
                                    ..
                                } => {
                                    format!(
                                        "Attached Oracle thread on {thread_id}: {title} ({conversation_id})"
                                    )
                                }
                            }
                        });
                        self.chat_widget
                            .add_to_history(oracle_history_cell("Oracle", &message));
                    }
                    Err(err)
                        if err.to_string()
                            == self.oracle_remote_thread_mutation_blocked_message() =>
                    {
                        self.chat_widget.add_error_message(err.to_string());
                    }
                    Err(err) => self.chat_widget.add_error_message(format!(
                        "Failed to attach Oracle thread {conversation_id}: {err}"
                    )),
                }
            }
            Ok(OracleCommand::On) => {
                let already_enabled = !self.oracle_state.bindings.is_empty();
                let thread_id = self
                    .ensure_oracle_entrypoint_thread(tui.as_deref_mut(), app_server)
                    .await?;
                let status = if already_enabled {
                    format!(
                        "Oracle thread is enabled on {thread_id}. Requested model: {}. Use /oracle to browse or attach additional threads.",
                        self.oracle_state.model.display_name()
                    )
                } else {
                    format!(
                        "Oracle thread is enabled on {thread_id}. Requested model: {}.",
                        self.oracle_state.model.display_name()
                    )
                };
                self.oracle_state.last_status = Some(status.clone());
                self.persist_active_oracle_binding();
                self.chat_widget
                    .add_to_history(oracle_history_cell("Oracle", &status));
            }
            Ok(OracleCommand::Off) => {
                if self.oracle_state.bindings.is_empty() {
                    self.chat_widget.add_to_history(oracle_history_cell(
                        "Oracle",
                        "Oracle mode is already disabled.",
                    ));
                    return Ok(());
                }
                if let Some(tui) = tui {
                    self.disable_oracle_mode(tui, app_server).await?;
                    self.chat_widget.add_to_history(oracle_history_cell(
                        "Oracle",
                        "Oracle mode disabled. The Oracle thread is preserved as a closed transcript.",
                    ));
                } else {
                    self.oracle_state.bindings.clear();
                    self.oracle_state.oracle_thread_id = None;
                    self.oracle_state.last_status = Some("Oracle mode disabled.".to_string());
                    self.persist_active_oracle_binding();
                }
            }
            Ok(OracleCommand::Status) => {
                let status = self.oracle_state.status_message();
                self.chat_widget
                    .add_to_history(oracle_history_cell("Oracle Info", &status));
            }
            Ok(OracleCommand::Model(model)) => {
                if let Some(model) = model {
                    let status = self.set_oracle_model_preference(model).await;
                    self.chat_widget
                        .add_to_history(oracle_history_cell("Oracle Model", &status));
                } else {
                    let status = format!(
                        "Requested Oracle model: {}\nPhase: {}",
                        self.oracle_state.model.display_name(),
                        self.oracle_state.phase.description(),
                    );
                    self.chat_widget
                        .add_to_history(oracle_history_cell("Oracle Model", &status));
                }
            }
            Err(error) => self.chat_widget.add_error_message(error),
        }
        Ok(())
    }

    async fn enqueue_thread_oracle_workflow_event(
        &mut self,
        thread_id: ThreadId,
        event: OracleWorkflowThreadEvent,
    ) {
        let store = {
            let channel = self.ensure_thread_channel(thread_id);
            Arc::clone(&channel.store)
        };

        let should_render_now = {
            let mut guard = store.lock().await;
            guard.push_buffered_event(ThreadBufferedEvent::OracleWorkflowEvent(event.clone()));
            guard.active && self.oracle_thread_is_displayed(thread_id)
        };

        if should_render_now {
            self.handle_oracle_workflow_thread_event(event);
        }
    }

    async fn handle_routed_oracle_thread_closed(&mut self, thread_id: ThreadId) {
        let Some(oracle_thread_id) = self.find_oracle_thread_by_orchestrator_thread(thread_id)
        else {
            return;
        };
        self.activate_oracle_binding(oracle_thread_id);
        let blocker = "The orchestrator thread closed before reporting back.".to_string();
        self.mark_agent_picker_thread_closed(thread_id);
        self.thread_event_channels.remove(&thread_id);
        let checkpoint_pending = self
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .is_some_and(|binding| binding.pending_checkpoint_threads.contains(&thread_id));
        if checkpoint_pending {
            self.persist_active_oracle_binding();
            return;
        }
        self.clear_closed_orchestrator_thread_binding(oracle_thread_id, thread_id, &blocker);
        self.persist_active_oracle_binding();
        if self.oracle_state.phase != OracleSupervisorPhase::WaitingForOrchestrator {
            return;
        }
        self.oracle_state.phase = OracleSupervisorPhase::Idle;
        self.oracle_state.automatic_context_followups = 0;
        let message = format!(
            "The orchestrator thread {thread_id} closed before reporting back. Oracle supervision is idle again."
        );
        self.oracle_state.last_status = Some(message.clone());
        self.append_oracle_agent_turn(oracle_thread_id, &message)
            .await;
        self.persist_active_oracle_binding();
    }

    /// Eagerly fetches nickname and role for receiver threads referenced by a collab notification.
    ///
    /// This runs on every buffered thread notification before it reaches rendering. For each
    /// receiver thread id that the navigation cache does not yet have metadata for, it issues a
    /// `thread/read` RPC and registers the result in both `AgentNavigationState` and the
    /// `ChatWidget` metadata map. Threads that already have a nickname or role cached are skipped,
    /// so the cost is at most one RPC per thread over the lifetime of a session.
    ///
    /// Failures are logged and silently ignored -- the worst outcome is that a rendered item shows
    /// a thread id instead of a human-readable name, which is the same behavior the TUI had before
    /// this change.

    async fn run_startup_ui_action(&mut self, action: StartupUiAction) -> Result<()> {
        match action {
            StartupUiAction::OpenOraclePicker => {
                self.oracle_picker_remote_list_pending = false;
                self.oracle_picker_remote_list_notice = None;
                self.show_oracle_picker(Vec::new(), /*include_new_thread*/ true);
                Ok(())
            }
        }
    }

    fn sort_oracle_thread_entries(remote_threads: &mut [OracleBrokerThreadEntry]) {
        remote_threads.sort_by(|a, b| {
            b.is_current
                .cmp(&a.is_current)
                .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
        });
    }

    fn next_oracle_picker_remote_list_request_id(&mut self) -> u64 {
        self.oracle_picker_remote_list_request_id =
            self.oracle_picker_remote_list_request_id.wrapping_add(1);
        if self.oracle_picker_remote_list_request_id == 0 {
            self.oracle_picker_remote_list_request_id = 1;
        }
        self.oracle_picker_remote_list_request_id
    }

    fn schedule_oracle_picker_remote_list_tick(&self, request_id: u64) {
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            tx.send(AppEvent::OraclePickerRemoteListTick { request_id });
        });
    }

    fn handle_oracle_picker_remote_list_tick(&mut self, request_id: u64) {
        if request_id != self.oracle_picker_remote_list_request_id
            || !self.oracle_picker_remote_list_pending
        {
            return;
        }
        self.oracle_picker_remote_list_tick = self.oracle_picker_remote_list_tick.wrapping_add(1);
        if self.refresh_oracle_picker_if_open() {
            self.schedule_oracle_picker_remote_list_tick(request_id);
        } else {
            self.oracle_picker_remote_list_pending = false;
        }
    }

    fn handle_oracle_picker_remote_threads_loaded(
        &mut self,
        request_id: u64,
        result: Result<Vec<OracleBrokerThreadEntry>, String>,
    ) {
        if request_id != self.oracle_picker_remote_list_request_id {
            return;
        }
        self.oracle_picker_remote_list_pending = false;
        match result {
            Ok(remote_threads) => {
                self.oracle_picker_remote_list_notice = None;
                self.show_oracle_picker(remote_threads, /*include_new_thread*/ true);
            }
            Err(err) => {
                self.oracle_picker_remote_list_notice = Some(err);
                self.show_oracle_picker(Vec::new(), /*include_new_thread*/ true);
            }
        }
    }

    #[cfg(test)]
    fn spawn_oracle_picker_remote_list_task(&mut self, request_id: u64) {
        let hooks = Arc::clone(&self.oracle_test_broker_hooks);
        let followup_session = self.preferred_oracle_followup_session();
        let list_timeout = self.oracle_picker_remote_list_timeout();
        let tx = self.app_event_tx.clone();
        if let Some((_repo, broker)) = self.oracle_broker.take() {
            tokio::spawn(async move {
                let _ = broker.shutdown().await;
            });
        }
        tokio::spawn(async move {
            let result = tokio::time::timeout(list_timeout, async move {
                let delay = hooks
                    .lock()
                    .expect("oracle test broker hooks")
                    .list_thread_delay
                    .take();
                if let Some(delay) = delay {
                    tokio::time::sleep(delay).await;
                }
                let result = {
                    let mut hooks = hooks.lock().expect("oracle test broker hooks");
                    let result = hooks.list_thread_results.pop_front();
                    if result.is_some() {
                        hooks.list_thread_calls += 1;
                        hooks.list_thread_followup_sessions.push(followup_session);
                    }
                    result
                };
                let mut remote_threads = match result {
                    Some(result) => result?,
                    None => Vec::new(),
                };
                Self::sort_oracle_thread_entries(remote_threads.as_mut_slice());
                Ok(remote_threads)
            })
            .await
            .unwrap_or_else(|_| {
                Err(format!(
                    "Timed out after {} while discovering remote Oracle threads.",
                    Self::oracle_timeout_message(list_timeout)
                ))
            });
            tx.send(AppEvent::OraclePickerRemoteThreadsLoaded { request_id, result });
        });
    }

    #[cfg(not(test))]
    fn spawn_oracle_picker_remote_list_task(&mut self, request_id: u64) {
        let tx = self.app_event_tx.clone();
        let list_timeout = self.oracle_picker_remote_list_timeout();
        let followup_session = self.preferred_oracle_followup_session_for_anchor(
            self.oracle_followup_session_anchor_thread_id(),
        );
        let oracle_repo_result = find_oracle_repo(self.chat_widget.config_ref().cwd.as_path())
            .ok_or_else(|| Self::oracle_repo_not_found_message().to_string());
        let broker_result = oracle_repo_result.and_then(|oracle_repo| {
            let cached_broker = match self.oracle_broker.take() {
                Some((cached_repo, broker))
                    if Self::cached_oracle_broker_matches(
                        cached_repo.as_path(),
                        oracle_repo.as_path(),
                        &broker,
                    ) =>
                {
                    Some(broker)
                }
                Some((cached_repo, broker)) => {
                    self.oracle_broker = Some((cached_repo, broker));
                    None
                }
                None => None,
            };
            cached_broker
                .map(Ok)
                .unwrap_or_else(|| spawn_oracle_broker(oracle_repo.as_path()))
        });
        tokio::spawn(async move {
            let result = match broker_result {
                Ok(broker) => {
                    let list_result =
                        tokio::time::timeout(list_timeout, broker.list_threads(followup_session))
                            .await;
                    match list_result {
                        Ok(Ok(mut remote_threads)) => {
                            Self::sort_oracle_thread_entries(remote_threads.as_mut_slice());
                            let _ = broker.shutdown().await;
                            Ok(remote_threads)
                        }
                        Ok(Err(err)) => {
                            let _ = broker.shutdown().await;
                            Err(err)
                        }
                        Err(_) => {
                            broker.abort_and_wait().await;
                            Err(format!(
                                "Timed out after {} while discovering remote Oracle threads.",
                                Self::oracle_timeout_message(list_timeout)
                            ))
                        }
                    }
                }
                Err(err) => Err(err),
            };
            tx.send(AppEvent::OraclePickerRemoteThreadsLoaded { request_id, result });
        });
    }

    fn oracle_picker_remote_list_timeout(&self) -> Duration {
        if let Some(timeout) =
            Self::oracle_duration_from_env_ms("CODEX_ORACLE_PICKER_REMOTE_LIST_TIMEOUT_MS")
        {
            return timeout;
        }
        if cfg!(test) {
            Duration::from_millis(50)
        } else {
            Duration::from_secs(120)
        }
    }

    fn oracle_remote_thread_mutation_timeout(&self) -> Duration {
        if let Some(timeout) =
            Self::oracle_duration_from_env_ms("CODEX_ORACLE_REMOTE_THREAD_MUTATION_TIMEOUT_MS")
        {
            return timeout;
        }
        if cfg!(test) {
            Duration::from_millis(50)
        } else {
            Duration::from_secs(20)
        }
    }

    fn oracle_duration_from_env_ms(key: &str) -> Option<Duration> {
        let millis = std::env::var(key).ok()?.parse::<u64>().ok()?;
        (millis > 0).then(|| Duration::from_millis(millis))
    }

    fn oracle_timeout_message(duration: Duration) -> String {
        if duration.as_secs() > 0 {
            format!("{} seconds", duration.as_secs())
        } else {
            format!("{} ms", duration.as_millis())
        }
    }

    async fn oracle_new_remote_thread(&mut self) -> Result<OracleBrokerThreadOpenResponse> {
        let timeout = self.oracle_remote_thread_mutation_timeout();
        match tokio::time::timeout(timeout, async {
            #[cfg(test)]
            let delay = self
                .oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .new_thread_delay
                .take();
            #[cfg(test)]
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            #[cfg(test)]
            if let Some(result) = {
                let followup_session = self.preferred_oracle_followup_session_for_anchor(
                    self.oracle_followup_session_anchor_thread_id(),
                );
                let mut hooks = self
                    .oracle_test_broker_hooks
                    .lock()
                    .expect("oracle test broker hooks");
                let result = hooks.new_thread_results.pop_front();
                if result.is_some() {
                    hooks.new_thread_calls += 1;
                    hooks.new_thread_followup_sessions.push(followup_session);
                }
                result
            } {
                return result.map_err(|err| color_eyre::eyre::eyre!(err));
            }
            let Some(oracle_repo) = find_oracle_repo(self.chat_widget.config_ref().cwd.as_path())
            else {
                return Err(color_eyre::eyre::eyre!(
                    "{}",
                    Self::oracle_repo_not_found_message()
                ));
            };
            let broker = self.ensure_oracle_broker(oracle_repo.as_path())?;
            let followup_session = self.require_oracle_followup_session_for_anchor(
                self.oracle_followup_session_anchor_thread_id(),
            )?;
            broker
                .new_thread(Some(followup_session))
                .await
                .map_err(|err| color_eyre::eyre::eyre!(err))
        })
        .await
        {
            Ok(result) => result,
            Err(_) => {
                self.shutdown_oracle_broker_await(true).await;
                Err(color_eyre::eyre::eyre!(
                    "Timed out after {} while creating a remote Oracle thread.",
                    Self::oracle_timeout_message(timeout)
                ))
            }
        }
    }

    async fn oracle_attach_remote_thread(
        &mut self,
        conversation_id: String,
        conversation_url: Option<String>,
        anchor_thread_id: Option<ThreadId>,
    ) -> Result<OracleBrokerThreadOpenResponse> {
        let timeout = self.oracle_remote_thread_mutation_timeout();
        match tokio::time::timeout(timeout, async {
            #[cfg(test)]
            let delay = self
                .oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .attach_thread_delay
                .take();
            #[cfg(test)]
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            #[cfg(test)]
            if let Some(result) = {
                let followup_session =
                    self.preferred_oracle_followup_session_for_anchor(anchor_thread_id);
                let mut hooks = self
                    .oracle_test_broker_hooks
                    .lock()
                    .expect("oracle test broker hooks");
                let conversation_id_for_log = conversation_id.clone();
                let result = hooks.attach_thread_results.pop_front();
                if result.is_some() {
                    hooks.attach_thread_calls.push(conversation_id_for_log);
                    hooks.attach_thread_followup_sessions.push(followup_session);
                }
                result
            } {
                return result.map_err(|err| color_eyre::eyre::eyre!(err));
            }
            let Some(oracle_repo) = find_oracle_repo(self.chat_widget.config_ref().cwd.as_path())
            else {
                return Err(color_eyre::eyre::eyre!(
                    "{}",
                    Self::oracle_repo_not_found_message()
                ));
            };
            let broker = self.ensure_oracle_broker(oracle_repo.as_path())?;
            let followup_session =
                self.preferred_oracle_followup_session_for_anchor(anchor_thread_id);
            broker
                .attach_thread(conversation_id, conversation_url, followup_session)
                .await
                .map_err(|err| color_eyre::eyre::eyre!(err))
        })
        .await
        {
            Ok(result) => result,
            Err(_) => {
                self.shutdown_oracle_broker_await(true).await;
                Err(color_eyre::eyre::eyre!(
                    "Timed out after {} while attaching a remote Oracle thread.",
                    Self::oracle_timeout_message(timeout)
                ))
            }
        }
    }

    async fn oracle_fetch_remote_thread_history(
        &mut self,
        thread_id: ThreadId,
    ) -> Result<OracleBrokerThreadHistoryResponse> {
        let conversation_id = self
            .oracle_state
            .bindings
            .get(&thread_id)
            .and_then(|binding| binding.conversation_id.clone())
            .ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "Oracle thread {thread_id} is not attached to a remote conversation."
                )
            })?;
        #[cfg(test)]
        if let Some(result) = {
            let followup_session = self.oracle_followup_session_for_thread(thread_id);
            let mut hooks = self
                .oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks");
            let result = hooks.thread_history_results.pop_front();
            if result.is_some() {
                hooks
                    .thread_history_followup_sessions
                    .push(followup_session);
            }
            result
        } {
            let response = result.map_err(|err| color_eyre::eyre::eyre!(err))?;
            if response.conversation_id.as_deref() != Some(conversation_id.as_str()) {
                return Err(color_eyre::eyre::eyre!(
                    "Oracle returned history for conversation {} while {} was requested.",
                    response.conversation_id.as_deref().unwrap_or("unknown"),
                    conversation_id
                ));
            }
            if let Some(session_id) = response.session_id.clone() {
                self.set_oracle_thread_broker_session(thread_id, Some(session_id), true);
            }
            return Ok(response);
        }
        let Some(oracle_repo) = find_oracle_repo(self.chat_widget.config_ref().cwd.as_path())
        else {
            return Err(color_eyre::eyre::eyre!(
                "{}",
                Self::oracle_repo_not_found_message()
            ));
        };
        let broker = self.ensure_oracle_broker(oracle_repo.as_path())?;
        let followup_session = self
            .ensure_oracle_followup_session_for_run(thread_id)
            .await?
            .ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "Oracle needs an attached local browser session before remote thread history is available."
                )
            })?;
        let history_limit = self.oracle_thread_history_limit();
        let fetch_history = |followup_session: String| async {
            broker
                .thread_history(
                    Some(followup_session),
                    conversation_id.clone(),
                    history_limit,
                )
                .await
        };
        let response = match fetch_history(followup_session.clone()).await {
            Ok(response) => response,
            Err(err) if Self::oracle_thread_history_requires_reattach(err.as_str()) => {
                self.set_oracle_thread_broker_session_id(thread_id, None);
                let retry_session = self
                    .ensure_oracle_followup_session_for_run(thread_id)
                    .await?
                    .ok_or_else(|| {
                        color_eyre::eyre::eyre!(
                            "Oracle needs an attached local browser session before remote thread history is available."
                        )
                    })?;
                fetch_history(retry_session)
                    .await
                    .map_err(|retry_err| color_eyre::eyre::eyre!(retry_err))?
            }
            Err(err) => return Err(color_eyre::eyre::eyre!(err)),
        };
        if response.conversation_id.as_deref() != Some(conversation_id.as_str()) {
            return Err(color_eyre::eyre::eyre!(
                "Oracle returned history for conversation {} while {} was requested.",
                response.conversation_id.as_deref().unwrap_or("unknown"),
                conversation_id
            ));
        }
        if let Some(session_id) = response.session_id.clone() {
            self.set_oracle_thread_broker_session(thread_id, Some(session_id), true);
        }
        Ok(response)
    }

    async fn open_oracle_picker(&mut self) -> Result<()> {
        self.persist_active_oracle_binding();
        self.oracle_picker_show_info = false;
        self.oracle_picker_remote_list_notice = None;
        if self.oracle_browser_is_busy() {
            self.oracle_picker_remote_list_pending = false;
            self.chat_widget.add_info_message(
                "Oracle is mid-turn; showing attached local Oracle threads only until the current browser step finishes."
                    .to_string(),
                None,
            );
            self.show_oracle_picker(Vec::new(), /*include_new_thread*/ false);
            return Ok(());
        }
        let request_id = self.next_oracle_picker_remote_list_request_id();
        self.oracle_picker_remote_threads.clear();
        self.oracle_picker_include_new_thread = true;
        self.oracle_picker_remote_list_pending = true;
        self.oracle_picker_remote_list_tick = 0;
        self.show_oracle_picker(Vec::new(), /*include_new_thread*/ true);
        self.schedule_oracle_picker_remote_list_tick(request_id);
        self.spawn_oracle_picker_remote_list_task(request_id);
        Ok(())
    }

    fn show_oracle_picker(
        &mut self,
        remote_threads: Vec<OracleBrokerThreadEntry>,
        include_new_thread: bool,
    ) {
        self.persist_active_oracle_binding();
        self.oracle_picker_remote_threads = remote_threads;
        self.oracle_picker_include_new_thread = include_new_thread;

        let mut initial_selected_idx = self
            .chat_widget
            .selected_index_for_active_view(ORACLE_SELECTION_VIEW_ID);
        let mut items = Vec::new();

        let mut local_oracle_ids = self
            .oracle_state
            .bindings
            .keys()
            .copied()
            .collect::<Vec<_>>();
        local_oracle_ids.sort_by_cached_key(|thread_id| {
            (
                self.oracle_picker_thread_title(*thread_id)
                    .to_ascii_lowercase(),
                thread_id.to_string(),
            )
        });
        for thread_id in local_oracle_ids {
            let is_current = self.active_thread_id == Some(thread_id);
            if initial_selected_idx.is_none() && is_current {
                initial_selected_idx = Some(items.len());
            }
            let binding = self.oracle_state.bindings.get(&thread_id);
            items.push(SelectionItem {
                name: format!("Attached: {}", self.oracle_picker_thread_title(thread_id)),
                is_current,
                actions: vec![Box::new({
                    move |tx| tx.send(AppEvent::SelectAgentThread(thread_id))
                })],
                dismiss_on_select: true,
                search_value: Some(format!(
                    "attached oracle {} {} {} {}",
                    thread_id,
                    binding
                        .and_then(|binding| binding.conversation_id.as_deref())
                        .unwrap_or(""),
                    binding
                        .and_then(|binding| binding.remote_title.as_deref())
                        .unwrap_or(""),
                    self.oracle_thread_label(thread_id),
                )),
                ..Default::default()
            });
        }

        let mut visible_remote_count = 0usize;
        for remote in &self.oracle_picker_remote_threads {
            if self
                .find_unique_oracle_thread_by_conversation_id(remote.conversation_id.as_str())
                .ok()
                .flatten()
                .is_some()
            {
                continue;
            }
            let title = remote.title.trim();
            let display_title = if title.is_empty() {
                format!("Conversation {}", remote.conversation_id)
            } else {
                title.to_string()
            };
            items.push(SelectionItem {
                name: format!("Remote: {display_title}"),
                is_current: false,
                actions: vec![Box::new({
                    let conversation_id = remote.conversation_id.clone();
                    let title = display_title.clone();
                    let url = remote.url.clone();
                    move |tx| {
                        tx.send(AppEvent::OracleAttachThread {
                            conversation_id: conversation_id.clone(),
                            title: title.clone(),
                            url: url.clone(),
                        })
                    }
                })],
                dismiss_on_select: true,
                search_value: Some(format!(
                    "remote oracle {} {} {}",
                    remote.conversation_id,
                    display_title,
                    remote.url.as_deref().unwrap_or("")
                )),
                ..Default::default()
            });
            visible_remote_count += 1;
        }

        if self.oracle_picker_remote_list_pending {
            let frames = ["|", "/", "-", "\\"];
            let frame = frames[self.oracle_picker_remote_list_tick % frames.len()];
            items.push(SelectionItem {
                name: format!("Fetching remote threads {frame}"),
                description: Some(
                    "Browserbase/ChatGPT discovery is still running; local controls stay usable."
                        .to_string(),
                ),
                is_disabled: true,
                search_value: Some("fetching remote oracle threads".to_string()),
                ..Default::default()
            });
        } else if let Some(notice) = self.oracle_picker_remote_list_notice.clone() {
            items.push(SelectionItem {
                name: "Remote threads unavailable".to_string(),
                description: Some(notice),
                is_disabled: true,
                search_value: Some("remote oracle threads unavailable".to_string()),
                ..Default::default()
            });
        }

        if initial_selected_idx.is_none() {
            initial_selected_idx = items
                .iter()
                .position(|item| !item.is_disabled && item.disabled_reason.is_none());
        }

        let search_enabled = items
            .iter()
            .any(|item| !item.is_disabled && item.disabled_reason.is_none());

        let mut shortcuts = vec![
            SelectionShortcut {
                shortcuts: vec![key_hint::plain(KeyCode::Char('i'))],
                active_while_searching: false,
                action: Box::new(
                    |_: &mut crate::bottom_pane::ListSelectionView, tx: &AppEventSender| {
                        tx.send(AppEvent::OraclePickerToggleInfo)
                    },
                ),
            },
            SelectionShortcut {
                shortcuts: vec![key_hint::plain(KeyCode::Tab)],
                active_while_searching: false,
                action: Box::new(
                    |_: &mut crate::bottom_pane::ListSelectionView, tx: &AppEventSender| {
                        tx.send(AppEvent::OraclePickerToggleModel)
                    },
                ),
            },
        ];
        if self.oracle_picker_include_new_thread {
            shortcuts.push(SelectionShortcut {
                shortcuts: vec![key_hint::plain(KeyCode::Char('n'))],
                active_while_searching: false,
                action: Box::new(
                    |view: &mut crate::bottom_pane::ListSelectionView, tx: &AppEventSender| {
                        view.dismiss();
                        tx.send(AppEvent::OracleCreateThread)
                    },
                ),
            });
        }
        if search_enabled {
            shortcuts.push(SelectionShortcut {
                shortcuts: vec![key_hint::plain(KeyCode::Char('s'))],
                active_while_searching: false,
                action: Box::new(
                    |view: &mut crate::bottom_pane::ListSelectionView, _: &AppEventSender| {
                        view.begin_search()
                    },
                ),
            });
        }

        let attached_count = self.oracle_state.bindings.len();
        let picker_is_active = self
            .chat_widget
            .selection_view_is_active(ORACLE_SELECTION_VIEW_ID);
        if picker_is_active {
            let _ = self.chat_widget.replace_selection_view_if_active(
                ORACLE_SELECTION_VIEW_ID,
                SelectionViewParams {
                    view_id: Some(ORACLE_SELECTION_VIEW_ID),
                    title: Some("Oracle".to_string()),
                    subtitle: Some(
                        self.oracle_picker_subtitle(attached_count, visible_remote_count),
                    ),
                    footer_note: Some(self.oracle_picker_hotkeys_line(
                        self.oracle_picker_include_new_thread,
                        search_enabled,
                    )),
                    footer_hint: Some(standard_popup_hint_line()),
                    items,
                    shortcuts,
                    initial_selected_idx,
                    is_searchable: search_enabled,
                    search_requires_activation: search_enabled,
                    search_placeholder: search_enabled
                        .then_some("Search Oracle threads".to_string()),
                    col_width_mode: ColumnWidthMode::AutoAllRows,
                    ..Default::default()
                },
            );
        } else {
            self.chat_widget.show_selection_view(SelectionViewParams {
                view_id: Some(ORACLE_SELECTION_VIEW_ID),
                title: Some("Oracle".to_string()),
                subtitle: Some(self.oracle_picker_subtitle(attached_count, visible_remote_count)),
                footer_note: Some(self.oracle_picker_hotkeys_line(
                    self.oracle_picker_include_new_thread,
                    search_enabled,
                )),
                footer_hint: Some(standard_popup_hint_line()),
                items,
                shortcuts,
                initial_selected_idx,
                is_searchable: search_enabled,
                search_requires_activation: search_enabled,
                search_placeholder: search_enabled.then_some("Search Oracle threads".to_string()),
                col_width_mode: ColumnWidthMode::AutoAllRows,
                ..Default::default()
            });
        }
        crate::session_log::log_oracle_picker_render(
            Some(self.oracle_picker_remote_list_request_id),
            self.oracle_picker_remote_list_pending,
            visible_remote_count,
            self.oracle_picker_include_new_thread,
        );
    }

    fn oracle_remote_thread_mutation_blocked_message(&self) -> &'static str {
        "Oracle is currently waiting on a browser turn. Finish or cancel that turn before creating or attaching a different remote Oracle thread."
    }

    fn ensure_oracle_remote_thread_mutation_allowed(&self) -> Result<()> {
        if self.oracle_browser_is_busy() {
            return Err(color_eyre::eyre::eyre!(
                "{}",
                self.oracle_remote_thread_mutation_blocked_message()
            ));
        }
        Ok(())
    }

    async fn bind_oracle_remote_thread(
        &mut self,
        remote: OracleBrokerThreadOpenResponse,
    ) -> Result<ThreadId> {
        let session_id = remote.session_id.clone();
        let thread_id = if let Some(existing) = remote
            .conversation_id
            .as_deref()
            .map(|conversation_id| {
                self.find_unique_oracle_thread_by_conversation_id(conversation_id)
            })
            .transpose()?
            .flatten()
        {
            existing
        } else {
            self.create_visible_oracle_thread().await
        };
        let conversation_id = remote.conversation_id.clone();
        let title = remote.title.clone();
        self.set_oracle_remote_metadata(thread_id, conversation_id, Some(title));
        self.set_oracle_thread_broker_session(thread_id, session_id, true);
        self.activate_oracle_binding(thread_id);
        self.upsert_agent_picker_thread(
            thread_id,
            Some(self.oracle_thread_label(thread_id)),
            Some("supervisor".to_string()),
            /*is_closed*/ false,
        );
        self.sync_oracle_thread_session(thread_id).await;
        Ok(thread_id)
    }

    async fn create_oracle_thread_binding(&mut self) -> Result<OracleNewThreadBinding> {
        self.ensure_oracle_remote_thread_mutation_allowed()?;
        if self.preferred_oracle_followup_session().is_none() {
            let thread_id = self.create_visible_oracle_thread().await;
            self.oracle_state.last_status = Some(format!(
                "Created new Oracle thread on {thread_id}. The hidden Oracle browser session will attach on the first turn."
            ));
            self.persist_active_oracle_binding();
            return Ok(OracleNewThreadBinding {
                thread_id,
                title: self.oracle_thread_label(thread_id),
                attached_remote: false,
            });
        }
        let remote = self.oracle_new_remote_thread().await?;
        let title = remote.title.clone();
        let thread_id = self.bind_oracle_remote_thread(remote).await?;
        self.oracle_state.last_status = Some(format!("Attached new Oracle thread on {thread_id}."));
        self.persist_active_oracle_binding();
        Ok(OracleNewThreadBinding {
            thread_id,
            title,
            attached_remote: true,
        })
    }

    fn oracle_new_thread_history_message(binding: &OracleNewThreadBinding) -> String {
        if binding.attached_remote {
            format!(
                "Attached new Oracle thread on {}: {}",
                binding.thread_id, binding.title
            )
        } else {
            format!(
                "Created new Oracle thread on {}. The hidden Oracle browser session will attach on the first turn.",
                binding.thread_id
            )
        }
    }

    async fn attach_oracle_thread_binding(
        &mut self,
        conversation_id: String,
        conversation_url: Option<String>,
        title_hint: Option<String>,
    ) -> Result<OracleAttachThreadBinding> {
        self.ensure_oracle_remote_thread_mutation_allowed()?;
        let previous_oracle_state = self.oracle_state.clone();
        let requested_conversation_url = conversation_url.clone();
        let mut anchor_thread_id = self.oracle_followup_session_anchor_thread_id();
        if let Some(thread_id) =
            self.find_unique_oracle_thread_by_conversation_id(conversation_id.as_str())?
        {
            if self.oracle_followup_session_for_thread(thread_id).is_some()
                && !self.oracle_broker_thread_session_requires_rebind(thread_id)
            {
                self.activate_oracle_binding(thread_id);
                self.sync_oracle_thread_session(thread_id).await;
                self.oracle_state.last_status = Some(format!(
                    "Reattached existing local Oracle thread {thread_id}."
                ));
                self.persist_active_oracle_binding();
                let title = title_hint
                    .or_else(|| {
                        self.oracle_state
                            .bindings
                            .get(&thread_id)
                            .and_then(|binding| binding.remote_title.clone())
                    })
                    .filter(|title| !title.trim().is_empty())
                    .unwrap_or_else(|| format!("Conversation {conversation_id}"));
                return Ok(OracleAttachThreadBinding::ReattachedLocal {
                    thread_id,
                    title,
                    conversation_id,
                });
            }
            self.activate_oracle_binding(thread_id);
            anchor_thread_id = Some(thread_id);
        }

        let remote = self
            .oracle_attach_remote_thread(
                conversation_id.clone(),
                conversation_url,
                anchor_thread_id,
            )
            .await
            .inspect_err(|_| {
                self.oracle_state = previous_oracle_state.clone();
            })?;
        if remote.conversation_id.as_deref() != Some(conversation_id.as_str()) {
            self.oracle_state = previous_oracle_state.clone();
            return Err(color_eyre::eyre::eyre!(
                "Oracle attached conversation {} while {} was requested.",
                remote.conversation_id.as_deref().unwrap_or("unknown"),
                conversation_id
            ));
        }
        if !Self::requested_oracle_conversation_url_matches_remote(
            conversation_id.as_str(),
            requested_conversation_url.as_deref(),
            remote.url.as_deref(),
        ) {
            self.oracle_state = previous_oracle_state.clone();
            return Err(color_eyre::eyre::eyre!(
                "Oracle attached thread {} while {} was requested; refusing to bind because the returned thread URL/scope does not match the requested conversation target.",
                remote.url.as_deref().unwrap_or("unknown"),
                requested_conversation_url.as_deref().unwrap_or("unknown"),
            ));
        }
        let title = remote.title.clone();
        let history = remote.history.clone();
        let thread_id = self.bind_oracle_remote_thread(remote).await?;
        self.oracle_state.last_status = Some(format!("Attached Oracle thread on {thread_id}."));
        self.persist_active_oracle_binding();
        Ok(OracleAttachThreadBinding::AttachedRemote {
            thread_id,
            title,
            conversation_id,
            history,
        })
    }

    async fn set_oracle_model_preference(&mut self, model: OracleModelPreset) -> String {
        self.oracle_state.model = model;
        let oracle_thread_ids = self
            .oracle_state
            .bindings
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for thread_id in oracle_thread_ids {
            self.sync_oracle_thread_session(thread_id).await;
        }
        let any_busy = self
            .oracle_state
            .bindings
            .values()
            .any(|binding| binding.phase.is_busy());
        let status = if any_busy {
            format!(
                "Oracle model preference set to {}. The current in-flight step will finish with its existing request. The next Oracle browser run will switch to the new model automatically.",
                model.display_name()
            )
        } else {
            format!(
                "Oracle model preference set to {}. The next Oracle browser run will switch to that model automatically.",
                model.display_name()
            )
        };
        self.oracle_state.last_status = Some(status.clone());
        self.persist_active_oracle_binding();
        status
    }

    pub(crate) async fn handle_tui_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: TuiEvent,
    ) -> Result<AppRunControl> {
        let terminal_resize_reflow_enabled = self.terminal_resize_reflow_enabled();
        if terminal_resize_reflow_enabled && matches!(event, TuiEvent::Draw | TuiEvent::Resize) {
            self.handle_draw_pre_render(tui)?;
        } else if matches!(event, TuiEvent::Draw | TuiEvent::Resize) {
            let size = tui.terminal.size()?;
            if size != tui.terminal.last_known_screen_size {
                self.refresh_status_line();
            }
        }

        if self.overlay.is_some() {
            let _ = self.handle_backtrack_overlay_event(tui, event).await?;
        } else {
            match event {
                TuiEvent::Key(key_event) => {
                    self.handle_key_event(tui, app_server, key_event).await;
                }
                TuiEvent::Paste(pasted) => {
                    // Many terminals convert newlines to \r when pasting (e.g., iTerm2),
                    // but tui-textarea expects \n. Normalize CR to LF.
                    // [tui-textarea]: https://github.com/rhysd/tui-textarea/blob/4d18622eeac13b309e0ff6a55a46ac6706da68cf/src/textarea.rs#L782-L783
                    // [iTerm2]: https://github.com/gnachman/iTerm2/blob/5d0c0d9f68523cbd0494dad5422998964a2ecd8d/sources/iTermPasteHelper.m#L206-L216
                    let pasted = pasted.replace("\r", "\n");
                    self.chat_widget.handle_paste(pasted);
                }
                TuiEvent::Draw | TuiEvent::Resize => {
                    if self.backtrack_render_pending {
                        self.backtrack_render_pending = false;
                        self.render_transcript_once(tui);
                    }
                    self.chat_widget.maybe_post_pending_notification(tui);
                    if self
                        .chat_widget
                        .handle_paste_burst_tick(tui.frame_requester())
                    {
                        return Ok(AppRunControl::Continue);
                    }
                    // Allow widgets to process any pending timers before rendering.
                    self.chat_widget.pre_draw_tick();
                    let desired_height =
                        self.chat_widget.desired_height(tui.terminal.size()?.width);
                    if terminal_resize_reflow_enabled {
                        tui.draw_with_resize_reflow(desired_height, |frame| {
                            self.chat_widget.render(frame.area(), frame.buffer);
                            if let Some((x, y)) = self.chat_widget.cursor_pos(frame.area()) {
                                frame.set_cursor_position((x, y));
                            }
                        })?;
                    } else {
                        tui.draw(desired_height, |frame| {
                            self.chat_widget.render(frame.area(), frame.buffer);
                            if let Some((x, y)) = self.chat_widget.cursor_pos(frame.area()) {
                                frame.set_cursor_position((x, y));
                            }
                        })?;
                    }
                    if self.chat_widget.external_editor_state() == ExternalEditorState::Requested {
                        self.chat_widget
                            .set_external_editor_state(ExternalEditorState::Active);
                        self.app_event_tx.send(AppEvent::LaunchExternalEditor);
                    }
                }
            }
        }
        Ok(AppRunControl::Continue)
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Err(err) = self.chat_widget.clear_managed_terminal_title() {
            tracing::debug!(error = %err, "failed to clear terminal title on app drop");
        }
    }
}

#[cfg(test)]
pub(super) mod test_support;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod carried_tests {
    use super::background_requests::McpInventoryMaps;
    use super::background_requests::build_feedback_upload_params;
    use super::background_requests::hide_cli_only_plugin_marketplaces;
    use super::background_requests::mcp_inventory_maps_from_statuses;
    use super::*;
    use crate::app_backtrack::BacktrackSelection;
    use crate::app_backtrack::BacktrackState;
    use crate::app_backtrack::user_count;

    use crate::chatwidget::ChatWidgetInit;
    use crate::chatwidget::create_initial_user_message;
    use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
    use crate::chatwidget::tests::set_chatgpt_auth;
    use crate::chatwidget::tests::set_fast_mode_test_catalog;
    use crate::file_search::FileSearchManager;
    use crate::history_cell::AgentMessageCell;
    use crate::history_cell::HistoryCell;
    use crate::history_cell::UserHistoryCell;
    use crate::history_cell::new_session_info;
    use crate::multi_agents::AgentPickerThreadEntry;
    use assert_matches::assert_matches;

    use crate::legacy_core::config::ConfigBuilder;
    use crate::legacy_core::config::ConfigOverrides;
    use codex_app_server_protocol::AdditionalFileSystemPermissions;
    use codex_app_server_protocol::AdditionalNetworkPermissions;
    use codex_app_server_protocol::AdditionalPermissionProfile;
    use codex_app_server_protocol::AgentMessageDeltaNotification;
    use codex_app_server_protocol::CommandExecutionRequestApprovalParams;
    use codex_app_server_protocol::ConfigWarningNotification;
    use codex_app_server_protocol::HookCompletedNotification;
    use codex_app_server_protocol::HookEventName as AppServerHookEventName;
    use codex_app_server_protocol::HookExecutionMode as AppServerHookExecutionMode;
    use codex_app_server_protocol::HookHandlerType as AppServerHookHandlerType;
    use codex_app_server_protocol::HookOutputEntry as AppServerHookOutputEntry;
    use codex_app_server_protocol::HookOutputEntryKind as AppServerHookOutputEntryKind;
    use codex_app_server_protocol::HookRunStatus as AppServerHookRunStatus;
    use codex_app_server_protocol::HookRunSummary as AppServerHookRunSummary;
    use codex_app_server_protocol::HookScope as AppServerHookScope;
    use codex_app_server_protocol::HookStartedNotification;
    use codex_app_server_protocol::JSONRPCErrorError;
    use codex_app_server_protocol::McpServerStartupState;
    use codex_app_server_protocol::McpServerStatusUpdatedNotification;
    use codex_app_server_protocol::NetworkApprovalContext as AppServerNetworkApprovalContext;
    use codex_app_server_protocol::NetworkApprovalProtocol as AppServerNetworkApprovalProtocol;
    use codex_app_server_protocol::NetworkPolicyAmendment as AppServerNetworkPolicyAmendment;
    use codex_app_server_protocol::NetworkPolicyRuleAction as AppServerNetworkPolicyRuleAction;
    use codex_app_server_protocol::NonSteerableTurnKind as AppServerNonSteerableTurnKind;
    use codex_app_server_protocol::PermissionsRequestApprovalParams;
    use codex_app_server_protocol::PluginMarketplaceEntry;
    use codex_app_server_protocol::RequestId as AppServerRequestId;
    use codex_app_server_protocol::ServerNotification;
    use codex_app_server_protocol::ServerRequest;
    use codex_app_server_protocol::Thread;
    use codex_app_server_protocol::ThreadClosedNotification;
    use codex_app_server_protocol::ThreadItem;
    use codex_app_server_protocol::ThreadStartedNotification;
    use codex_app_server_protocol::ThreadTokenUsage;
    use codex_app_server_protocol::ThreadTokenUsageUpdatedNotification;
    use codex_app_server_protocol::TokenUsageBreakdown;
    use codex_app_server_protocol::ToolRequestUserInputParams;
    use codex_app_server_protocol::Turn;
    use codex_app_server_protocol::TurnCompletedNotification;
    use codex_app_server_protocol::TurnError as AppServerTurnError;
    use codex_app_server_protocol::TurnStartedNotification;
    use codex_app_server_protocol::TurnStatus;
    use codex_app_server_protocol::UserInput as AppServerUserInput;
    use codex_app_server_protocol::WarningNotification;
    use codex_otel::SessionTelemetry;
    use codex_protocol::ThreadId;
    use codex_protocol::config_types::CollaborationMode;
    use codex_protocol::config_types::CollaborationModeMask;
    use codex_protocol::config_types::ModeKind;
    use codex_protocol::config_types::Settings;
    use codex_protocol::mcp::Tool;
    use codex_protocol::models::FileSystemPermissions;
    use codex_protocol::models::NetworkPermissions;
    use codex_protocol::models::PermissionProfile;
    use codex_protocol::protocol::AskForApproval;
    use codex_protocol::protocol::Event;
    use codex_protocol::protocol::EventMsg;
    use codex_protocol::protocol::McpAuthStatus;
    use codex_protocol::protocol::NetworkApprovalContext;
    use codex_protocol::protocol::NetworkApprovalProtocol;
    use codex_protocol::protocol::RolloutItem;
    use codex_protocol::protocol::RolloutLine;
    use codex_protocol::protocol::SandboxPolicy;
    use codex_protocol::protocol::SessionConfiguredEvent;
    use codex_protocol::protocol::SessionSource;
    use codex_protocol::protocol::TurnContextItem;
    use codex_protocol::request_permissions::RequestPermissionProfile;
    use codex_protocol::user_input::TextElement;
    use codex_protocol::user_input::UserInput;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use crossterm::event::KeyModifiers;
    use insta::assert_snapshot;
    use pretty_assertions::assert_eq;
    use ratatui::prelude::Line;
    use serial_test::serial;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use tempfile::tempdir;
    use tokio::time;

    mod model_catalog {
        include!("app/tests/model_catalog.rs");
    }

    fn test_absolute_path(path: &str) -> AbsolutePathBuf {
        AbsolutePathBuf::try_from(PathBuf::from(path)).expect("absolute test path")
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.original {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn hide_cli_only_plugin_marketplaces_removes_openai_bundled() {
        let mut response = PluginListResponse {
            marketplaces: vec![
                PluginMarketplaceEntry {
                    name: "openai-bundled".to_string(),
                    path: Some(test_absolute_path("/marketplaces/openai-bundled")),
                    interface: None,
                    plugins: Vec::new(),
                },
                PluginMarketplaceEntry {
                    name: "openai-curated".to_string(),
                    path: Some(test_absolute_path("/marketplaces/openai-curated")),
                    interface: None,
                    plugins: Vec::new(),
                },
            ],
            marketplace_load_errors: Vec::new(),
            featured_plugin_ids: Vec::new(),
        };

        hide_cli_only_plugin_marketplaces(&mut response);

        assert_eq!(
            response.marketplaces,
            vec![PluginMarketplaceEntry {
                name: "openai-curated".to_string(),
                path: Some(test_absolute_path("/marketplaces/openai-curated")),
                interface: None,
                plugins: Vec::new(),
            }]
        );
    }

    #[test]
    fn commit_tick_coalescing_leaves_room_for_interrupt_ops() {
        let (tx, mut rx) = unbounded_channel();
        let sender = AppEventSender::new(tx);
        let pending = AtomicBool::new(false);

        assert!(try_enqueue_commit_tick(&sender, &pending));
        assert!(
            !try_enqueue_commit_tick(&sender, &pending),
            "duplicate commit ticks should be suppressed while one is pending"
        );

        sender.interrupt();

        assert!(matches!(rx.try_recv(), Ok(AppEvent::CommitTick)));
        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::CodexOp(Op::Interrupt))
        ));
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn normalize_harness_overrides_resolves_relative_add_dirs() -> Result<()> {
        let temp_dir = tempdir()?;
        let base_cwd = temp_dir.path().join("base").abs();
        std::fs::create_dir_all(base_cwd.as_path())?;

        let overrides = ConfigOverrides {
            additional_writable_roots: vec![PathBuf::from("rel")],
            ..Default::default()
        };
        let normalized = normalize_harness_overrides_for_cwd(overrides, &base_cwd)?;

        assert_eq!(
            normalized.additional_writable_roots,
            vec![base_cwd.join("rel").into_path_buf()]
        );
        Ok(())
    }

    #[test]
    fn mcp_inventory_maps_prefix_tool_names_by_server() {
        let statuses = vec![
            McpServerStatus {
                name: "docs".to_string(),
                tools: HashMap::from([(
                    "list".to_string(),
                    Tool {
                        description: None,
                        name: "list".to_string(),
                        title: None,
                        input_schema: serde_json::json!({"type": "object"}),
                        output_schema: None,
                        annotations: None,
                        icons: None,
                        meta: None,
                    },
                )]),
                resources: Vec::new(),
                resource_templates: Vec::new(),
                auth_status: codex_app_server_protocol::McpAuthStatus::Unsupported,
            },
            McpServerStatus {
                name: "disabled".to_string(),
                tools: HashMap::new(),
                resources: Vec::new(),
                resource_templates: Vec::new(),
                auth_status: codex_app_server_protocol::McpAuthStatus::Unsupported,
            },
        ];

        let (tools, resources, resource_templates, auth_statuses): McpInventoryMaps =
            mcp_inventory_maps_from_statuses(statuses);
        let mut resource_names = resources.keys().cloned().collect::<Vec<_>>();
        resource_names.sort();
        let mut template_names = resource_templates.keys().cloned().collect::<Vec<_>>();
        template_names.sort();

        assert_eq!(
            tools.keys().cloned().collect::<Vec<_>>(),
            vec!["mcp__docs__list".to_string()]
        );
        assert_eq!(resource_names, vec!["disabled", "docs"]);
        assert_eq!(template_names, vec!["disabled", "docs"]);
        assert_eq!(
            auth_statuses.get("disabled"),
            Some(&McpAuthStatus::Unsupported)
        );
    }

    #[tokio::test]
    async fn handle_mcp_inventory_result_clears_committed_loading_cell() {
        let mut app = make_test_app().await;
        app.transcript_cells
            .push(Arc::new(history_cell::new_mcp_inventory_loading(
                /*animations_enabled*/ false,
            )));

        app.handle_mcp_inventory_result(
            Ok(vec![McpServerStatus {
                name: "docs".to_string(),
                tools: HashMap::new(),
                resources: Vec::new(),
                resource_templates: Vec::new(),
                auth_status: codex_app_server_protocol::McpAuthStatus::Unsupported,
            }]),
            McpServerStatusDetail::ToolsAndAuthOnly,
        );

        assert_eq!(app.transcript_cells.len(), 0);
    }

    #[test]
    fn startup_waiting_gate_is_only_for_fresh_or_exit_session_selection() {
        assert_eq!(
            App::should_wait_for_initial_session(&SessionSelection::StartFresh),
            true
        );
        assert_eq!(
            App::should_wait_for_initial_session(&SessionSelection::Exit),
            true
        );
        assert_eq!(
            App::should_wait_for_initial_session(&SessionSelection::Resume(
                crate::resume_picker::SessionTarget {
                    path: Some(PathBuf::from("/tmp/restore")),
                    thread_id: ThreadId::new(),
                }
            )),
            false
        );
        assert_eq!(
            App::should_wait_for_initial_session(&SessionSelection::Fork(
                crate::resume_picker::SessionTarget {
                    path: Some(PathBuf::from("/tmp/fork")),
                    thread_id: ThreadId::new(),
                }
            )),
            false
        );
    }

    #[test]
    fn startup_waiting_gate_holds_active_thread_events_until_primary_thread_configured() {
        let mut wait_for_initial_session =
            App::should_wait_for_initial_session(&SessionSelection::StartFresh);
        assert_eq!(wait_for_initial_session, true);
        assert_eq!(
            App::should_handle_active_thread_events(
                wait_for_initial_session,
                /*has_active_thread_receiver*/ true
            ),
            false
        );

        assert_eq!(
            App::should_stop_waiting_for_initial_session(
                wait_for_initial_session,
                /*primary_thread_id*/ None
            ),
            false
        );
        if App::should_stop_waiting_for_initial_session(
            wait_for_initial_session,
            Some(ThreadId::new()),
        ) {
            wait_for_initial_session = false;
        }
        assert_eq!(wait_for_initial_session, false);

        assert_eq!(
            App::should_handle_active_thread_events(
                wait_for_initial_session,
                /*has_active_thread_receiver*/ true
            ),
            true
        );
    }

    #[test]
    fn startup_waiting_gate_not_applied_for_resume_or_fork_session_selection() {
        let wait_for_resume = App::should_wait_for_initial_session(&SessionSelection::Resume(
            crate::resume_picker::SessionTarget {
                path: Some(PathBuf::from("/tmp/restore")),
                thread_id: ThreadId::new(),
            },
        ));
        assert_eq!(
            App::should_handle_active_thread_events(
                wait_for_resume,
                /*has_active_thread_receiver*/ true
            ),
            true
        );
        let wait_for_fork = App::should_wait_for_initial_session(&SessionSelection::Fork(
            crate::resume_picker::SessionTarget {
                path: Some(PathBuf::from("/tmp/fork")),
                thread_id: ThreadId::new(),
            },
        ));
        assert_eq!(
            App::should_handle_active_thread_events(
                wait_for_fork,
                /*has_active_thread_receiver*/ true
            ),
            true
        );
    }

    #[tokio::test]
    async fn ignore_same_thread_resume_reports_noop_for_current_thread() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        app.chat_widget.handle_thread_session(session.clone());
        app.thread_event_channels.insert(
            thread_id,
            ThreadEventChannel::new_with_session(
                THREAD_EVENT_CHANNEL_CAPACITY,
                session,
                Vec::new(),
            ),
        );
        app.activate_thread_channel(thread_id).await;
        while app_event_rx.try_recv().is_ok() {}

        let ignored = app.ignore_same_thread_resume(&crate::resume_picker::SessionTarget {
            path: Some(test_path_buf("/tmp/project")),
            thread_id,
        });

        assert!(ignored);
        let cell = match app_event_rx.try_recv() {
            Ok(AppEvent::InsertHistoryCell(cell)) => cell,
            other => panic!("expected info message after same-thread resume, saw {other:?}"),
        };
        let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
        assert!(rendered.contains(&format!(
            "Already viewing {}.",
            test_path_display("/tmp/project")
        )));
    }

    #[tokio::test]
    async fn ignore_same_thread_resume_allows_reattaching_displayed_inactive_thread() {
        let mut app = make_test_app().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        app.chat_widget.handle_thread_session(session);

        let ignored = app.ignore_same_thread_resume(&crate::resume_picker::SessionTarget {
            path: Some(test_path_buf("/tmp/project")),
            thread_id,
        });

        assert!(!ignored);
        assert!(app.transcript_cells.is_empty());
    }

    #[tokio::test]
    async fn enqueue_primary_thread_session_replays_buffered_approval_after_attach() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let approval_request =
            exec_approval_request(thread_id, "turn-1", "call-1", /*approval_id*/ None);

        assert_eq!(
            app.pending_app_server_requests
                .note_server_request(&approval_request),
            None
        );
        app.enqueue_primary_thread_request(approval_request).await?;
        app.enqueue_primary_thread_session(
            test_thread_session(thread_id, test_path_buf("/tmp/project")),
            Vec::new(),
        )
        .await?;

        let rx = app
            .active_thread_rx
            .as_mut()
            .expect("primary thread receiver should be active");
        let event = time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("timed out waiting for buffered approval event")
            .expect("channel closed unexpectedly");

        assert!(matches!(
            &event,
            ThreadBufferedEvent::Request(ServerRequest::CommandExecutionRequestApproval {
                params,
                ..
            }) if params.turn_id == "turn-1"
        ));

        app.handle_thread_event_now(event);
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

        while let Ok(app_event) = app_event_rx.try_recv() {
            if let AppEvent::SubmitThreadOp {
                thread_id: op_thread_id,
                ..
            } = app_event
            {
                assert_eq!(op_thread_id, thread_id);
                return Ok(());
            }
        }

        panic!("expected approval action to submit a thread-scoped op");
    }

    #[tokio::test]
    async fn resolved_buffered_approval_does_not_become_actionable_after_drain() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let approval_request =
            exec_approval_request(thread_id, "turn-1", "call-1", /*approval_id*/ None);

        app.enqueue_primary_thread_session(
            test_thread_session(thread_id, test_path_buf("/tmp/project")),
            Vec::new(),
        )
        .await?;
        while app_event_rx.try_recv().is_ok() {}

        assert_eq!(
            app.pending_app_server_requests
                .note_server_request(&approval_request),
            None
        );
        app.enqueue_thread_request(thread_id, approval_request)
            .await?;

        let resolved = app
            .pending_app_server_requests
            .resolve_notification(&AppServerRequestId::Integer(1))
            .expect("matching app-server request should resolve");
        app.chat_widget.dismiss_app_server_request(&resolved);
        while app_event_rx.try_recv().is_ok() {}

        let rx = app
            .active_thread_rx
            .as_mut()
            .expect("primary thread receiver should be active");
        let event = time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("timed out waiting for buffered approval event")
            .expect("channel closed unexpectedly");

        assert!(matches!(
            &event,
            ThreadBufferedEvent::Request(ServerRequest::CommandExecutionRequestApproval {
                params,
                ..
            }) if params.turn_id == "turn-1"
        ));

        app.handle_thread_event_now(event);
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

        while let Ok(app_event) = app_event_rx.try_recv() {
            assert!(
                !matches!(app_event, AppEvent::SubmitThreadOp { .. }),
                "resolved buffered approval should not become actionable"
            );
        }

        Ok(())
    }

    #[tokio::test]
    async fn enqueue_primary_thread_session_replays_turns_before_initial_prompt_submit()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let initial_prompt = "follow-up after replay".to_string();
        let config = app.config.clone();
        let model = crate::legacy_core::test_support::get_model_offline(config.model.as_deref());
        app.chat_widget = ChatWidget::new_with_app_event(ChatWidgetInit {
            config,
            frame_requester: crate::tui::FrameRequester::test_dummy(),
            app_event_tx: app.app_event_tx.clone(),
            initial_user_message: create_initial_user_message(
                Some(initial_prompt.clone()),
                Vec::new(),
                Vec::new(),
            ),
            enhanced_keys_supported: false,
            has_chatgpt_account: false,
            model_catalog: app.model_catalog.clone(),
            feedback: codex_feedback::CodexFeedback::new(),
            is_first_run: false,
            status_account_display: None,
            initial_plan_type: None,
            model: Some(model),
            startup_tooltip_override: None,
            status_line_invalid_items_warned: app.status_line_invalid_items_warned.clone(),
            terminal_title_invalid_items_warned: app.terminal_title_invalid_items_warned.clone(),
            session_telemetry: app.session_telemetry.clone(),
        });

        app.enqueue_primary_thread_session(
            test_thread_session(thread_id, test_path_buf("/tmp/project")),
            vec![test_turn(
                "turn-1",
                TurnStatus::Completed,
                vec![ThreadItem::UserMessage {
                    id: "user-1".to_string(),
                    content: vec![AppServerUserInput::Text {
                        text: "earlier prompt".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
            )],
        )
        .await?;

        let mut saw_replayed_answer = false;
        let mut submitted_items = None;
        while let Ok(event) = app_event_rx.try_recv() {
            match event {
                AppEvent::InsertHistoryCell(cell) => {
                    let transcript = lines_to_single_string(&cell.transcript_lines(/*width*/ 80));
                    saw_replayed_answer |= transcript.contains("earlier prompt");
                }
                AppEvent::SubmitThreadOp {
                    thread_id: op_thread_id,
                    op: Op::UserTurn { items, .. },
                } => {
                    assert_eq!(op_thread_id, thread_id);
                    submitted_items = Some(items);
                }
                AppEvent::CodexOp(Op::UserTurn { items, .. }) => {
                    submitted_items = Some(items);
                }
                _ => {}
            }
        }
        assert!(
            saw_replayed_answer,
            "expected replayed history before initial prompt submit"
        );
        assert_eq!(
            submitted_items,
            Some(vec![UserInput::Text {
                text: initial_prompt,
                text_elements: Vec::new(),
            }])
        );

        Ok(())
    }

    #[tokio::test]
    async fn reset_thread_event_state_aborts_listener_tasks() {
        struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

        impl Drop for NotifyOnDrop {
            fn drop(&mut self) {
                if let Some(tx) = self.0.take() {
                    let _ = tx.send(());
                }
            }
        }

        let mut app = make_test_app().await;
        let thread_id = ThreadId::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let _notify_on_drop = NotifyOnDrop(Some(dropped_tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        app.thread_event_listener_tasks.insert(thread_id, handle);
        started_rx
            .await
            .expect("listener task should report it started");

        app.reset_thread_event_state();

        assert_eq!(app.thread_event_listener_tasks.is_empty(), true);
        time::timeout(Duration::from_millis(50), dropped_rx)
            .await
            .expect("timed out waiting for listener task abort")
            .expect("listener task drop notification should succeed");
    }

    #[tokio::test]
    async fn history_lookup_response_is_routed_to_requesting_thread() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();

        let handled = app
            .try_handle_local_history_op(
                thread_id,
                &Op::GetHistoryEntryRequest {
                    offset: 0,
                    log_id: 1,
                }
                .into(),
            )
            .await?;

        assert!(handled);

        let app_event = tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv())
            .await
            .expect("history lookup should emit an app event")
            .expect("app event channel should stay open");

        let AppEvent::ThreadHistoryEntryResponse {
            thread_id: routed_thread_id,
            event,
        } = app_event
        else {
            panic!("expected thread-routed history response");
        };
        assert_eq!(routed_thread_id, thread_id);
        assert_eq!(event.offset, 0);
        assert_eq!(event.log_id, 1);
        assert!(event.entry.is_none());

        Ok(())
    }

    #[tokio::test]
    async fn enqueue_thread_event_does_not_block_when_channel_full() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = ThreadId::new();
        app.thread_event_channels
            .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));
        app.set_thread_active(thread_id, /*active*/ true).await;

        let event = thread_closed_notification(thread_id);

        app.enqueue_thread_notification(thread_id, event.clone())
            .await?;
        time::timeout(
            Duration::from_millis(50),
            app.enqueue_thread_notification(thread_id, event),
        )
        .await
        .expect("enqueue_thread_notification blocked on a full channel")?;

        let mut rx = app
            .thread_event_channels
            .get_mut(&thread_id)
            .expect("missing thread channel")
            .receiver
            .take()
            .expect("missing receiver");

        time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("timed out waiting for first event")
            .expect("channel closed unexpectedly");
        time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("timed out waiting for second event")
            .expect("channel closed unexpectedly");

        Ok(())
    }

    #[tokio::test]
    async fn replay_thread_snapshot_restores_draft_and_queued_input() {
        let mut app = make_test_app().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        app.thread_event_channels.insert(
            thread_id,
            ThreadEventChannel::new_with_session(
                THREAD_EVENT_CHANNEL_CAPACITY,
                session.clone(),
                Vec::new(),
            ),
        );
        app.activate_thread_channel(thread_id).await;
        app.chat_widget.handle_thread_session(session.clone());

        app.chat_widget
            .apply_external_edit("draft prompt".to_string());
        app.chat_widget.submit_user_message_with_mode(
            "queued follow-up".to_string(),
            CollaborationModeMask {
                name: "Default".to_string(),
                mode: None,
                model: None,
                reasoning_effort: None,
                developer_instructions: None,
            },
        );
        let expected_input_state = app
            .chat_widget
            .capture_thread_input_state()
            .expect("expected thread input state");

        app.store_active_thread_receiver().await;

        let snapshot = {
            let channel = app
                .thread_event_channels
                .get(&thread_id)
                .expect("thread channel should exist");
            let store = channel.store.lock().await;
            assert_eq!(store.input_state, Some(expected_input_state));
            store.snapshot()
        };

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;

        app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ true);

        assert_eq!(app.chat_widget.composer_text_with_pending(), "draft prompt");
        assert!(app.chat_widget.queued_user_message_texts().is_empty());
        while let Ok(op) = new_op_rx.try_recv() {
            assert!(
                !matches!(op, Op::UserTurn { .. }),
                "draft-only replay should not auto-submit queued input"
            );
        }
    }

    #[tokio::test]
    async fn active_turn_id_for_thread_uses_snapshot_turns() {
        let mut app = make_test_app().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        app.thread_event_channels.insert(
            thread_id,
            ThreadEventChannel::new_with_session(
                THREAD_EVENT_CHANNEL_CAPACITY,
                session,
                vec![test_turn("turn-1", TurnStatus::InProgress, Vec::new())],
            ),
        );

        assert_eq!(
            app.active_turn_id_for_thread(thread_id).await,
            Some("turn-1".to_string())
        );
    }

    #[tokio::test]
    async fn replayed_turn_complete_submits_restored_queued_follow_up() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        app.chat_widget.handle_thread_session(session.clone());
        app.chat_widget.handle_server_notification(
            turn_started_notification(thread_id, "turn-1"),
            /*replay_kind*/ None,
        );
        app.chat_widget.handle_server_notification(
            agent_message_delta_notification(thread_id, "turn-1", "agent-1", "streaming"),
            /*replay_kind*/ None,
        );
        app.chat_widget
            .apply_external_edit("queued follow-up".to_string());
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let input_state = app
            .chat_widget
            .capture_thread_input_state()
            .expect("expected queued follow-up state");

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_thread_session(session.clone());
        while new_op_rx.try_recv().is_ok() {}
        app.replay_thread_snapshot(
            ThreadEventSnapshot {
                session: None,
                turns: Vec::new(),
                events: vec![ThreadBufferedEvent::Notification(
                    turn_completed_notification(thread_id, "turn-1", TurnStatus::Completed),
                )],
                replay_entries: Vec::new(),
                input_state: Some(input_state),
            },
            /*resume_restored_queue*/ true,
        );

        match next_user_turn_op(&mut new_op_rx) {
            Op::UserTurn { items, .. } => assert_eq!(
                items,
                vec![UserInput::Text {
                    text: "queued follow-up".to_string(),
                    text_elements: Vec::new(),
                }]
            ),
            other => panic!("expected queued follow-up submission, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_only_thread_keeps_restored_queue_visible() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        app.chat_widget.handle_thread_session(session.clone());
        app.chat_widget.handle_server_notification(
            turn_started_notification(thread_id, "turn-1"),
            /*replay_kind*/ None,
        );
        app.chat_widget.handle_server_notification(
            agent_message_delta_notification(thread_id, "turn-1", "agent-1", "streaming"),
            /*replay_kind*/ None,
        );
        app.chat_widget
            .apply_external_edit("queued follow-up".to_string());
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let input_state = app
            .chat_widget
            .capture_thread_input_state()
            .expect("expected queued follow-up state");

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_thread_session(session.clone());
        while new_op_rx.try_recv().is_ok() {}

        app.replay_thread_snapshot(
            ThreadEventSnapshot {
                session: None,
                turns: Vec::new(),
                events: vec![ThreadBufferedEvent::Notification(
                    turn_completed_notification(thread_id, "turn-1", TurnStatus::Completed),
                )],
                replay_entries: Vec::new(),
                input_state: Some(input_state),
            },
            /*resume_restored_queue*/ false,
        );

        assert_eq!(
            app.chat_widget.queued_user_message_texts(),
            vec!["queued follow-up".to_string()]
        );
        assert!(
            new_op_rx.try_recv().is_err(),
            "replay-only threads should not auto-submit restored queue"
        );
    }

    #[tokio::test]
    async fn replay_thread_snapshot_keeps_queue_when_running_state_only_comes_from_snapshot() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        app.chat_widget.handle_thread_session(session.clone());
        app.chat_widget.handle_server_notification(
            turn_started_notification(thread_id, "turn-1"),
            /*replay_kind*/ None,
        );
        app.chat_widget.handle_server_notification(
            agent_message_delta_notification(thread_id, "turn-1", "agent-1", "streaming"),
            /*replay_kind*/ None,
        );
        app.chat_widget
            .apply_external_edit("queued follow-up".to_string());
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let input_state = app
            .chat_widget
            .capture_thread_input_state()
            .expect("expected queued follow-up state");

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_thread_session(session.clone());
        while new_op_rx.try_recv().is_ok() {}

        app.replay_thread_snapshot(
            ThreadEventSnapshot {
                session: None,
                turns: Vec::new(),
                events: vec![],
                replay_entries: Vec::new(),
                input_state: Some(input_state),
            },
            /*resume_restored_queue*/ true,
        );

        assert_eq!(
            app.chat_widget.queued_user_message_texts(),
            vec!["queued follow-up".to_string()]
        );
        assert!(
            new_op_rx.try_recv().is_err(),
            "restored queue should stay queued when replay did not prove the turn finished"
        );
    }

    #[tokio::test]
    async fn replay_thread_snapshot_in_progress_turn_restores_running_queue_state() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        app.chat_widget.handle_thread_session(session.clone());
        app.chat_widget.handle_server_notification(
            turn_started_notification(thread_id, "turn-1"),
            /*replay_kind*/ None,
        );
        app.chat_widget.handle_server_notification(
            agent_message_delta_notification(thread_id, "turn-1", "agent-1", "streaming"),
            /*replay_kind*/ None,
        );
        app.chat_widget
            .apply_external_edit("queued follow-up".to_string());
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let input_state = app
            .chat_widget
            .capture_thread_input_state()
            .expect("expected queued follow-up state");

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_thread_session(session.clone());
        while new_op_rx.try_recv().is_ok() {}

        app.replay_thread_snapshot(
            ThreadEventSnapshot {
                session: None,
                turns: vec![test_turn("turn-1", TurnStatus::InProgress, Vec::new())],
                events: Vec::new(),
                replay_entries: Vec::new(),
                input_state: Some(input_state),
            },
            /*resume_restored_queue*/ true,
        );

        assert_eq!(
            app.chat_widget.queued_user_message_texts(),
            vec!["queued follow-up".to_string()]
        );
        assert!(
            new_op_rx.try_recv().is_err(),
            "restored queue should stay queued while replayed turn is still running"
        );
    }

    #[tokio::test]
    async fn replay_thread_snapshot_in_progress_turn_restores_running_state_without_input_state() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        let (chat_widget, _app_event_tx, _rx, _new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_thread_session(session);

        app.replay_thread_snapshot(
            ThreadEventSnapshot {
                session: None,
                turns: vec![test_turn("turn-1", TurnStatus::InProgress, Vec::new())],
                events: Vec::new(),
                replay_entries: Vec::new(),
                input_state: None,
            },
            /*resume_restored_queue*/ false,
        );

        assert!(app.chat_widget.is_task_running_for_test());
    }

    #[tokio::test]
    async fn replay_thread_snapshot_does_not_submit_queue_before_replay_catches_up() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        app.chat_widget.handle_thread_session(session.clone());
        app.chat_widget.handle_server_notification(
            turn_started_notification(thread_id, "turn-1"),
            /*replay_kind*/ None,
        );
        app.chat_widget.handle_server_notification(
            agent_message_delta_notification(thread_id, "turn-1", "agent-1", "streaming"),
            /*replay_kind*/ None,
        );
        app.chat_widget
            .apply_external_edit("queued follow-up".to_string());
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let input_state = app
            .chat_widget
            .capture_thread_input_state()
            .expect("expected queued follow-up state");

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_thread_session(session.clone());
        while new_op_rx.try_recv().is_ok() {}

        app.replay_thread_snapshot(
            ThreadEventSnapshot {
                session: None,
                turns: Vec::new(),
                events: vec![
                    ThreadBufferedEvent::Notification(turn_completed_notification(
                        thread_id,
                        "turn-0",
                        TurnStatus::Completed,
                    )),
                    ThreadBufferedEvent::Notification(turn_started_notification(
                        thread_id, "turn-1",
                    )),
                ],
                replay_entries: Vec::new(),
                input_state: Some(input_state),
            },
            /*resume_restored_queue*/ true,
        );

        assert!(
            new_op_rx.try_recv().is_err(),
            "queued follow-up should stay queued until the latest turn completes"
        );
        assert_eq!(
            app.chat_widget.queued_user_message_texts(),
            vec!["queued follow-up".to_string()]
        );

        app.chat_widget.handle_server_notification(
            turn_completed_notification(thread_id, "turn-1", TurnStatus::Completed),
            /*replay_kind*/ None,
        );

        match next_user_turn_op(&mut new_op_rx) {
            Op::UserTurn { items, .. } => assert_eq!(
                items,
                vec![UserInput::Text {
                    text: "queued follow-up".to_string(),
                    text_elements: Vec::new(),
                }]
            ),
            other => panic!("expected queued follow-up submission, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_thread_snapshot_restores_pending_pastes_for_submit() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        app.thread_event_channels.insert(
            thread_id,
            ThreadEventChannel::new_with_session(
                THREAD_EVENT_CHANNEL_CAPACITY,
                session.clone(),
                Vec::new(),
            ),
        );
        app.activate_thread_channel(thread_id).await;
        app.chat_widget.handle_thread_session(session);

        let large = "x".repeat(1005);
        app.chat_widget.handle_paste(large.clone());
        let expected_input_state = app
            .chat_widget
            .capture_thread_input_state()
            .expect("expected thread input state");

        app.store_active_thread_receiver().await;

        let snapshot = {
            let channel = app
                .thread_event_channels
                .get(&thread_id)
                .expect("thread channel should exist");
            let store = channel.store.lock().await;
            assert_eq!(store.input_state, Some(expected_input_state));
            store.snapshot()
        };

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ true);

        assert_eq!(app.chat_widget.composer_text_with_pending(), large);

        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match next_user_turn_op(&mut new_op_rx) {
            Op::UserTurn { items, .. } => assert_eq!(
                items,
                vec![UserInput::Text {
                    text: large,
                    text_elements: Vec::new(),
                }]
            ),
            other => panic!("expected restored paste submission, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_thread_snapshot_restores_collaboration_mode_for_draft_submit() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        app.chat_widget.handle_thread_session(session.clone());
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::High));
        app.chat_widget
            .set_collaboration_mask(CollaborationModeMask {
                name: "Plan".to_string(),
                mode: Some(ModeKind::Plan),
                model: Some("gpt-restored".to_string()),
                reasoning_effort: Some(Some(ReasoningEffortConfig::High)),
                developer_instructions: None,
            });
        app.chat_widget
            .apply_external_edit("draft prompt".to_string());
        let input_state = app
            .chat_widget
            .capture_thread_input_state()
            .expect("expected draft input state");

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_thread_session(session.clone());
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::Low));
        app.chat_widget
            .set_collaboration_mask(CollaborationModeMask {
                name: "Default".to_string(),
                mode: Some(ModeKind::Default),
                model: Some("gpt-replacement".to_string()),
                reasoning_effort: Some(Some(ReasoningEffortConfig::Low)),
                developer_instructions: None,
            });
        while new_op_rx.try_recv().is_ok() {}

        app.replay_thread_snapshot(
            ThreadEventSnapshot {
                session: None,
                turns: Vec::new(),
                events: vec![],
                replay_entries: Vec::new(),
                input_state: Some(input_state),
            },
            /*resume_restored_queue*/ true,
        );
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match next_user_turn_op(&mut new_op_rx) {
            Op::UserTurn {
                items,
                model,
                effort,
                collaboration_mode,
                ..
            } => {
                assert_eq!(
                    items,
                    vec![UserInput::Text {
                        text: "draft prompt".to_string(),
                        text_elements: Vec::new(),
                    }]
                );
                assert_eq!(model, "gpt-restored".to_string());
                assert_eq!(effort, Some(ReasoningEffortConfig::High));
                assert_eq!(
                    collaboration_mode,
                    Some(CollaborationMode {
                        mode: ModeKind::Plan,
                        settings: Settings {
                            model: "gpt-restored".to_string(),
                            reasoning_effort: Some(ReasoningEffortConfig::High),
                            developer_instructions: None,
                        },
                    })
                );
            }
            other => panic!("expected restored draft submission, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_thread_snapshot_restores_collaboration_mode_without_input() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        app.chat_widget.handle_thread_session(session.clone());
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::High));
        app.chat_widget
            .set_collaboration_mask(CollaborationModeMask {
                name: "Plan".to_string(),
                mode: Some(ModeKind::Plan),
                model: Some("gpt-restored".to_string()),
                reasoning_effort: Some(Some(ReasoningEffortConfig::High)),
                developer_instructions: None,
            });
        let input_state = app
            .chat_widget
            .capture_thread_input_state()
            .expect("expected collaboration-only input state");

        let (chat_widget, _app_event_tx, _rx, _new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_thread_session(session.clone());
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::Low));
        app.chat_widget
            .set_collaboration_mask(CollaborationModeMask {
                name: "Default".to_string(),
                mode: Some(ModeKind::Default),
                model: Some("gpt-replacement".to_string()),
                reasoning_effort: Some(Some(ReasoningEffortConfig::Low)),
                developer_instructions: None,
            });

        app.replay_thread_snapshot(
            ThreadEventSnapshot {
                session: None,
                turns: Vec::new(),
                events: vec![],
                replay_entries: Vec::new(),
                input_state: Some(input_state),
            },
            /*resume_restored_queue*/ true,
        );

        assert_eq!(
            app.chat_widget.active_collaboration_mode_kind(),
            ModeKind::Plan
        );
        assert_eq!(app.chat_widget.current_model(), "gpt-restored");
        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::High)
        );
    }

    #[tokio::test]
    async fn replayed_interrupted_turn_restores_queued_input_to_composer() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        app.chat_widget.handle_thread_session(session.clone());
        app.chat_widget.handle_server_notification(
            turn_started_notification(thread_id, "turn-1"),
            /*replay_kind*/ None,
        );
        app.chat_widget.handle_server_notification(
            agent_message_delta_notification(thread_id, "turn-1", "agent-1", "streaming"),
            /*replay_kind*/ None,
        );
        app.chat_widget
            .apply_external_edit("queued follow-up".to_string());
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let input_state = app
            .chat_widget
            .capture_thread_input_state()
            .expect("expected queued follow-up state");

        let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
            make_chatwidget_manual_with_sender().await;
        app.chat_widget = chat_widget;
        app.chat_widget.handle_thread_session(session.clone());
        while new_op_rx.try_recv().is_ok() {}

        app.replay_thread_snapshot(
            ThreadEventSnapshot {
                session: None,
                turns: Vec::new(),
                events: vec![ThreadBufferedEvent::Notification(
                    turn_completed_notification(thread_id, "turn-1", TurnStatus::Interrupted),
                )],
                replay_entries: Vec::new(),
                input_state: Some(input_state),
            },
            /*resume_restored_queue*/ true,
        );

        assert_eq!(
            app.chat_widget.composer_text_with_pending(),
            "queued follow-up"
        );
        assert!(app.chat_widget.queued_user_message_texts().is_empty());
        assert!(
            new_op_rx.try_recv().is_err(),
            "replayed interrupted turns should restore queued input for editing, not submit it"
        );
    }

    #[tokio::test]
    async fn token_usage_update_refreshes_status_line_with_runtime_context_window() {
        let mut app = make_test_app().await;
        app.chat_widget
            .setup_status_line(vec![crate::bottom_pane::StatusLineItem::ContextWindowSize]);

        assert_eq!(app.chat_widget.status_line_text(), None);

        app.handle_thread_event_now(ThreadBufferedEvent::Notification(token_usage_notification(
            ThreadId::new(),
            "turn-1",
            Some(950_000),
        )));

        assert_eq!(
            app.chat_widget.status_line_text(),
            Some("950K window".into())
        );
    }

    #[tokio::test]
    async fn open_agent_picker_keeps_missing_threads_for_replay() -> Result<()> {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let thread_id = ThreadId::new();
        app.thread_event_channels
            .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));

        app.open_agent_picker(&mut app_server).await;

        assert_eq!(app.thread_event_channels.contains_key(&thread_id), true);
        assert_eq!(
            app.agent_navigation.get(&thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: None,
                agent_role: None,
                is_closed: true,
            })
        );
        assert_eq!(app.agent_navigation.ordered_thread_ids(), vec![thread_id]);
        Ok(())
    }

    #[tokio::test]
    async fn open_agent_picker_preserves_cached_metadata_for_replay_threads() -> Result<()> {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let thread_id = ThreadId::new();
        app.thread_event_channels
            .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));
        app.agent_navigation.upsert(
            thread_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            /*is_closed*/ true,
        );

        app.open_agent_picker(&mut app_server).await;

        assert_eq!(app.thread_event_channels.contains_key(&thread_id), true);
        assert_eq!(
            app.agent_navigation.get(&thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("explorer".to_string()),
                is_closed: true,
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn open_agent_picker_prunes_terminal_metadata_only_threads() -> Result<()> {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let thread_id = ThreadId::new();
        app.agent_navigation.upsert(
            thread_id,
            Some("Ghost".to_string()),
            Some("worker".to_string()),
            /*is_closed*/ false,
        );

        app.open_agent_picker(&mut app_server).await;

        assert_eq!(app.agent_navigation.get(&thread_id), None);
        assert!(app.agent_navigation.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn open_agent_picker_marks_terminal_read_errors_closed() -> Result<()> {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let thread_id = ThreadId::new();
        app.thread_event_channels
            .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));
        app.agent_navigation.upsert(
            thread_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            /*is_closed*/ false,
        );

        app.open_agent_picker(&mut app_server).await;

        assert_eq!(
            app.agent_navigation.get(&thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("explorer".to_string()),
                is_closed: true,
            })
        );
        Ok(())
    }

    #[test]
    fn terminal_thread_read_error_detection_matches_not_loaded_errors() {
        let err = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read failed: thread not loaded: thr_123"
        );

        assert!(App::is_terminal_thread_read_error(&err));
    }

    #[test]
    fn terminal_thread_read_error_detection_ignores_transient_failures() {
        let err = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read transport error: broken pipe"
        );

        assert!(!App::is_terminal_thread_read_error(&err));
    }

    #[test]
    fn closed_state_for_thread_read_error_preserves_live_state_without_cache_on_transient_error() {
        let err = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read transport error: broken pipe"
        );

        assert!(!App::closed_state_for_thread_read_error(
            &err, /*existing_is_closed*/ None
        ));
    }

    #[test]
    fn closed_state_for_thread_read_error_marks_terminal_uncached_threads_closed() {
        let err = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read failed: thread not loaded: thr_123"
        );

        assert!(App::closed_state_for_thread_read_error(
            &err, /*existing_is_closed*/ None
        ));
    }

    #[test]
    fn include_turns_fallback_detection_handles_unmaterialized_and_ephemeral_threads() {
        let unmaterialized = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read failed: thread thr_123 is not materialized yet; includeTurns is unavailable before first user message"
        );
        let ephemeral = color_eyre::eyre::eyre!(
            "thread/read failed during TUI session lookup: thread/read failed: ephemeral threads do not support includeTurns"
        );

        assert!(App::can_fallback_from_include_turns_error(&unmaterialized));
        assert!(App::can_fallback_from_include_turns_error(&ephemeral));
    }

    #[tokio::test]
    async fn open_agent_picker_marks_loaded_threads_open() -> Result<()> {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let started = app_server
            .start_thread(app.chat_widget.config_ref())
            .await?;
        let thread_id = started.session.thread_id;
        app.thread_event_channels
            .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));

        app.open_agent_picker(&mut app_server).await;

        assert_eq!(
            app.agent_navigation.get(&thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: None,
                agent_role: None,
                is_closed: false,
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn open_agent_picker_keeps_visible_oracle_thread_open() -> Result<()> {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let thread_id = app.ensure_visible_oracle_thread().await;

        app.open_agent_picker(&mut app_server).await;

        assert_eq!(
            app.agent_navigation.get(&thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: Some("Oracle".to_string()),
                agent_role: Some("supervisor".to_string()),
                is_closed: false,
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn open_agent_picker_enter_on_oracle_row_opens_workflow_picker() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;

        app.open_agent_picker(&mut app_server).await;
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::OpenOracleWorkflowPicker(selected_thread_id))
                if selected_thread_id == oracle_thread_id
        );
        Ok(())
    }

    #[tokio::test]
    async fn attach_live_thread_for_selection_rejects_empty_non_ephemeral_fallback_threads()
    -> Result<()> {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let started = app_server
            .start_thread(app.chat_widget.config_ref())
            .await?;
        let thread_id = started.session.thread_id;
        app.agent_navigation.upsert(
            thread_id,
            Some("Scout".to_string()),
            Some("worker".to_string()),
            /*is_closed*/ false,
        );

        let err = app
            .attach_live_thread_for_selection(&mut app_server, thread_id)
            .await
            .expect_err("empty fallback should not attach as a blank replay-only thread");

        assert_eq!(
            err.to_string(),
            format!("Agent thread {thread_id} is not yet available for replay or live attach.")
        );
        assert!(!app.thread_event_channels.contains_key(&thread_id));
        Ok(())
    }

    #[tokio::test]
    async fn attach_live_thread_for_selection_rejects_unmaterialized_fallback_threads() -> Result<()>
    {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let mut ephemeral_config = app.chat_widget.config_ref().clone();
        ephemeral_config.ephemeral = true;
        let started = app_server.start_thread(&ephemeral_config).await?;
        let thread_id = started.session.thread_id;
        app.agent_navigation.upsert(
            thread_id,
            Some("Scout".to_string()),
            Some("worker".to_string()),
            /*is_closed*/ false,
        );

        let err = app
            .attach_live_thread_for_selection(&mut app_server, thread_id)
            .await
            .expect_err("ephemeral fallback should not attach as a blank live thread");

        assert_eq!(
            err.to_string(),
            format!("Agent thread {thread_id} is not yet available for replay or live attach.")
        );
        assert!(!app.thread_event_channels.contains_key(&thread_id));
        Ok(())
    }

    #[tokio::test]
    async fn should_attach_live_thread_for_selection_skips_closed_metadata_only_threads() {
        let mut app = make_test_app().await;
        let thread_id = ThreadId::new();
        app.agent_navigation.upsert(
            thread_id,
            Some("Ghost".to_string()),
            Some("worker".to_string()),
            /*is_closed*/ true,
        );

        assert!(!app.should_attach_live_thread_for_selection(thread_id));

        app.agent_navigation.upsert(
            thread_id,
            Some("Ghost".to_string()),
            Some("worker".to_string()),
            /*is_closed*/ false,
        );
        assert!(app.should_attach_live_thread_for_selection(thread_id));

        app.thread_event_channels
            .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));
        assert!(!app.should_attach_live_thread_for_selection(thread_id));
    }

    #[tokio::test]
    async fn refresh_agent_picker_thread_liveness_prunes_closed_metadata_only_threads() -> Result<()>
    {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let thread_id = ThreadId::new();
        app.agent_navigation.upsert(
            thread_id,
            Some("Ghost".to_string()),
            Some("worker".to_string()),
            /*is_closed*/ false,
        );

        let is_available = app
            .refresh_agent_picker_thread_liveness(&mut app_server, thread_id)
            .await;

        assert!(!is_available);
        assert_eq!(app.agent_navigation.get(&thread_id), None);
        assert!(!app.thread_event_channels.contains_key(&thread_id));
        Ok(())
    }

    #[tokio::test]
    async fn refresh_agent_picker_thread_liveness_keeps_visible_oracle_threads_open() -> Result<()>
    {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let thread_id = app.create_visible_oracle_thread().await;
        app.agent_navigation.upsert(
            thread_id,
            Some("Oracle".to_string()),
            Some("supervisor".to_string()),
            /*is_closed*/ true,
        );

        let is_available = app
            .refresh_agent_picker_thread_liveness(&mut app_server, thread_id)
            .await;

        assert!(is_available);
        let entry = app
            .agent_navigation
            .get(&thread_id)
            .expect("oracle picker entry");
        assert!(!entry.is_closed);
        Ok(())
    }

    #[tokio::test]
    async fn open_agent_picker_prompts_to_enable_multi_agent_when_disabled() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let _ = app.config.features.disable(Feature::Collab);

        app.open_agent_picker(&mut app_server).await;
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::UpdateFeatureFlags { updates }) if updates == vec![(Feature::Collab, true)]
        );
        let cell = match app_event_rx.try_recv() {
            Ok(AppEvent::InsertHistoryCell(cell)) => cell,
            other => panic!("expected InsertHistoryCell event, got {other:?}"),
        };
        let rendered = cell
            .display_lines(/*width*/ 120)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Subagents will be enabled in the next session."));
        Ok(())
    }

    #[tokio::test]
    async fn update_memory_settings_persists_and_updates_widget_config() -> Result<()> {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;

        app.update_memory_settings_with_app_server(
            &mut app_server,
            /*use_memories*/ false,
            /*generate_memories*/ false,
        )
        .await;

        assert!(!app.config.memories.use_memories);
        assert!(!app.config.memories.generate_memories);
        assert!(!app.chat_widget.config_ref().memories.use_memories);
        assert!(!app.chat_widget.config_ref().memories.generate_memories);

        let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
        let config_value = toml::from_str::<TomlValue>(&config)?;
        let memories = config_value
            .as_table()
            .and_then(|table| table.get("memories"))
            .and_then(TomlValue::as_table)
            .expect("memories table should exist");
        assert_eq!(
            memories.get("use_memories"),
            Some(&TomlValue::Boolean(false))
        );
        assert_eq!(
            memories.get("generate_memories"),
            Some(&TomlValue::Boolean(false))
        );
        assert!(
            !memories.contains_key("disable_on_external_context")
                && !memories.contains_key("no_memories_if_mcp_or_web_search"),
            "the TUI menu should not write the external-context memory setting"
        );
        app_server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn update_memory_settings_updates_current_thread_memory_mode() -> Result<()> {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        // Seed the previous setting so this test exercises the thread-mode update path.
        app.config.memories.generate_memories = true;

        let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;
        let started = app_server.start_thread(&app.config).await?;
        let thread_id = started.session.thread_id;
        app.active_thread_id = Some(thread_id);

        app.update_memory_settings_with_app_server(
            &mut app_server,
            /*use_memories*/ true,
            /*generate_memories*/ false,
        )
        .await;

        let state_db = codex_state::StateRuntime::init(
            codex_home.path().to_path_buf(),
            app.config.model_provider_id.clone(),
        )
        .await
        .expect("state db should initialize");
        let memory_mode = state_db
            .get_thread_memory_mode(thread_id)
            .await
            .expect("thread memory mode should be readable");
        assert_eq!(memory_mode.as_deref(), Some("disabled"));

        app_server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn reset_memories_clears_local_memory_directories() -> Result<()> {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        app.config.sqlite_home = codex_home.path().to_path_buf();

        let memory_root = codex_home.path().join("memories");
        let extensions_root = codex_home.path().join("memories_extensions");
        std::fs::create_dir_all(memory_root.join("rollout_summaries"))?;
        std::fs::create_dir_all(&extensions_root)?;
        std::fs::write(memory_root.join("MEMORY.md"), "stale memory\n")?;
        std::fs::write(
            memory_root.join("rollout_summaries").join("stale.md"),
            "stale summary\n",
        )?;
        std::fs::write(extensions_root.join("stale.txt"), "stale extension\n")?;

        let mut app_server = crate::start_embedded_app_server_for_picker(&app.config).await?;

        app.reset_memories_with_app_server(&mut app_server).await;

        assert_eq!(std::fs::read_dir(&memory_root)?.count(), 0);
        assert_eq!(std::fs::read_dir(&extensions_root)?.count(), 0);

        app_server.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn update_feature_flags_enabling_guardian_selects_auto_review() -> Result<()> {
        let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        let auto_review = auto_review_mode();

        app.update_feature_flags(vec![(Feature::GuardianApproval, true)])
            .await;

        assert!(app.config.features.enabled(Feature::GuardianApproval));
        assert!(
            app.chat_widget
                .config_ref()
                .features
                .enabled(Feature::GuardianApproval)
        );
        assert_eq!(
            app.config.approvals_reviewer,
            auto_review.approvals_reviewer
        );
        assert_eq!(
            app.config.permissions.approval_policy.value(),
            auto_review.approval_policy
        );
        assert_eq!(
            app.chat_widget
                .config_ref()
                .permissions
                .approval_policy
                .value(),
            auto_review.approval_policy
        );
        assert_eq!(
            app.chat_widget.config_ref().legacy_sandbox_policy(),
            auto_review.sandbox_policy
        );
        assert_eq!(
            app.chat_widget.config_ref().approvals_reviewer,
            auto_review.approvals_reviewer
        );
        assert_eq!(app.runtime_approval_policy_override, None);
        assert_eq!(app.runtime_sandbox_policy_override, None);
        assert_eq!(
            op_rx.try_recv(),
            Ok(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: Some(auto_review.approval_policy),
                approvals_reviewer: Some(auto_review.approvals_reviewer),
                sandbox_policy: Some(auto_review.sandbox_policy.clone()),
                permission_profile: None,
                windows_sandbox_level: None,
                model: None,
                effort: None,
                summary: None,
                service_tier: None,
                collaboration_mode: None,
                personality: None,
            })
        );
        let cell = match app_event_rx.try_recv() {
            Ok(AppEvent::InsertHistoryCell(cell)) => cell,
            other => panic!("expected InsertHistoryCell event, got {other:?}"),
        };
        let rendered = cell
            .display_lines(/*width*/ 120)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Permissions updated to Auto-review"));

        let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
        assert!(config.contains("guardian_approval = true"));
        assert!(config.contains("approvals_reviewer = \"auto_review\""));
        assert!(config.contains("approval_policy = \"on-request\""));
        assert!(config.contains("sandbox_mode = \"workspace-write\""));
        Ok(())
    }

    #[tokio::test]
    async fn update_feature_flags_disabling_guardian_clears_review_policy_and_restores_default()
    -> Result<()> {
        let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        let config_toml_path = codex_home.path().join("config.toml").abs();
        let config_toml = "approvals_reviewer = \"guardian_subagent\"\napproval_policy = \"on-request\"\nsandbox_mode = \"workspace-write\"\n\n[features]\nguardian_approval = true\n";
        std::fs::write(config_toml_path.as_path(), config_toml)?;
        let user_config = toml::from_str::<TomlValue>(config_toml)?;
        app.config.config_layer_stack = app
            .config
            .config_layer_stack
            .with_user_config(&config_toml_path, user_config);
        app.config
            .features
            .set_enabled(Feature::GuardianApproval, /*enabled*/ true)?;
        app.chat_widget
            .set_feature_enabled(Feature::GuardianApproval, /*enabled*/ true);
        app.config.approvals_reviewer = ApprovalsReviewer::AutoReview;
        app.chat_widget
            .set_approvals_reviewer(ApprovalsReviewer::AutoReview);
        app.config
            .permissions
            .approval_policy
            .set(AskForApproval::OnRequest)?;
        app.config
            .set_legacy_sandbox_policy(SandboxPolicy::new_workspace_write_policy())?;
        app.chat_widget
            .set_approval_policy(AskForApproval::OnRequest);
        app.chat_widget
            .set_sandbox_policy(SandboxPolicy::new_workspace_write_policy())?;

        app.update_feature_flags(vec![(Feature::GuardianApproval, false)])
            .await;

        assert!(!app.config.features.enabled(Feature::GuardianApproval));
        assert!(
            !app.chat_widget
                .config_ref()
                .features
                .enabled(Feature::GuardianApproval)
        );
        assert_eq!(app.config.approvals_reviewer, ApprovalsReviewer::User);
        assert_eq!(
            app.config.permissions.approval_policy.value(),
            AskForApproval::OnRequest
        );
        assert_eq!(
            app.chat_widget.config_ref().approvals_reviewer,
            ApprovalsReviewer::User
        );
        assert_eq!(app.runtime_approval_policy_override, None);
        assert_eq!(
            op_rx.try_recv(),
            Ok(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: None,
                approvals_reviewer: Some(ApprovalsReviewer::User),
                sandbox_policy: None,
                permission_profile: None,
                windows_sandbox_level: None,
                model: None,
                effort: None,
                summary: None,
                service_tier: None,
                collaboration_mode: None,
                personality: None,
            })
        );
        let cell = match app_event_rx.try_recv() {
            Ok(AppEvent::InsertHistoryCell(cell)) => cell,
            other => panic!("expected InsertHistoryCell event, got {other:?}"),
        };
        let rendered = cell
            .display_lines(/*width*/ 120)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Permissions updated to Default"));

        let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
        assert!(!config.contains("guardian_approval = true"));
        assert!(!config.contains("approvals_reviewer ="));
        assert!(config.contains("approval_policy = \"on-request\""));
        assert!(config.contains("sandbox_mode = \"workspace-write\""));
        Ok(())
    }

    #[tokio::test]
    async fn update_feature_flags_enabling_guardian_overrides_explicit_manual_review_policy()
    -> Result<()> {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        let auto_review = auto_review_mode();
        let config_toml_path = codex_home.path().join("config.toml").abs();
        let config_toml = "approvals_reviewer = \"user\"\n";
        std::fs::write(config_toml_path.as_path(), config_toml)?;
        let user_config = toml::from_str::<TomlValue>(config_toml)?;
        app.config.config_layer_stack = app
            .config
            .config_layer_stack
            .with_user_config(&config_toml_path, user_config);
        app.config.approvals_reviewer = ApprovalsReviewer::User;
        app.chat_widget
            .set_approvals_reviewer(ApprovalsReviewer::User);

        app.update_feature_flags(vec![(Feature::GuardianApproval, true)])
            .await;

        assert!(app.config.features.enabled(Feature::GuardianApproval));
        assert_eq!(
            app.config.approvals_reviewer,
            auto_review.approvals_reviewer
        );
        assert_eq!(
            app.chat_widget.config_ref().approvals_reviewer,
            auto_review.approvals_reviewer
        );
        assert_eq!(
            app.config.permissions.approval_policy.value(),
            auto_review.approval_policy
        );
        assert_eq!(
            app.chat_widget.config_ref().legacy_sandbox_policy(),
            auto_review.sandbox_policy
        );
        assert_eq!(
            op_rx.try_recv(),
            Ok(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: Some(auto_review.approval_policy),
                approvals_reviewer: Some(auto_review.approvals_reviewer),
                sandbox_policy: Some(auto_review.sandbox_policy.clone()),
                permission_profile: None,
                windows_sandbox_level: None,
                model: None,
                effort: None,
                summary: None,
                service_tier: None,
                collaboration_mode: None,
                personality: None,
            })
        );

        let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
        assert!(config.contains("approvals_reviewer = \"auto_review\""));
        assert!(config.contains("guardian_approval = true"));
        assert!(config.contains("approval_policy = \"on-request\""));
        assert!(config.contains("sandbox_mode = \"workspace-write\""));
        Ok(())
    }

    #[tokio::test]
    async fn update_feature_flags_disabling_guardian_clears_manual_review_policy_without_history()
    -> Result<()> {
        let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        let config_toml_path = codex_home.path().join("config.toml").abs();
        let config_toml = "approvals_reviewer = \"user\"\napproval_policy = \"on-request\"\nsandbox_mode = \"workspace-write\"\n\n[features]\nguardian_approval = true\n";
        std::fs::write(config_toml_path.as_path(), config_toml)?;
        let user_config = toml::from_str::<TomlValue>(config_toml)?;
        app.config.config_layer_stack = app
            .config
            .config_layer_stack
            .with_user_config(&config_toml_path, user_config);
        app.config
            .features
            .set_enabled(Feature::GuardianApproval, /*enabled*/ true)?;
        app.chat_widget
            .set_feature_enabled(Feature::GuardianApproval, /*enabled*/ true);
        app.config.approvals_reviewer = ApprovalsReviewer::User;
        app.chat_widget
            .set_approvals_reviewer(ApprovalsReviewer::User);

        app.update_feature_flags(vec![(Feature::GuardianApproval, false)])
            .await;

        assert!(!app.config.features.enabled(Feature::GuardianApproval));
        assert_eq!(app.config.approvals_reviewer, ApprovalsReviewer::User);
        assert_eq!(
            app.chat_widget.config_ref().approvals_reviewer,
            ApprovalsReviewer::User
        );
        assert_eq!(
            op_rx.try_recv(),
            Ok(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: None,
                approvals_reviewer: Some(ApprovalsReviewer::User),
                sandbox_policy: None,
                permission_profile: None,
                windows_sandbox_level: None,
                model: None,
                effort: None,
                summary: None,
                service_tier: None,
                collaboration_mode: None,
                personality: None,
            })
        );
        assert!(
            app_event_rx.try_recv().is_err(),
            "manual review should not emit a permissions history update when the effective state stays default"
        );

        let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
        assert!(!config.contains("guardian_approval = true"));
        assert!(!config.contains("approvals_reviewer ="));
        Ok(())
    }

    #[tokio::test]
    async fn update_feature_flags_enabling_guardian_in_profile_sets_profile_auto_review_policy()
    -> Result<()> {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        let auto_review = auto_review_mode();
        app.active_profile = Some("guardian".to_string());
        let config_toml_path = codex_home.path().join("config.toml").abs();
        let config_toml = "profile = \"guardian\"\napprovals_reviewer = \"user\"\n";
        std::fs::write(config_toml_path.as_path(), config_toml)?;
        let user_config = toml::from_str::<TomlValue>(config_toml)?;
        app.config.config_layer_stack = app
            .config
            .config_layer_stack
            .with_user_config(&config_toml_path, user_config);
        app.config.approvals_reviewer = ApprovalsReviewer::User;
        app.chat_widget
            .set_approvals_reviewer(ApprovalsReviewer::User);

        app.update_feature_flags(vec![(Feature::GuardianApproval, true)])
            .await;

        assert!(app.config.features.enabled(Feature::GuardianApproval));
        assert_eq!(
            app.config.approvals_reviewer,
            auto_review.approvals_reviewer
        );
        assert_eq!(
            app.chat_widget.config_ref().approvals_reviewer,
            auto_review.approvals_reviewer
        );
        assert_eq!(
            op_rx.try_recv(),
            Ok(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: Some(auto_review.approval_policy),
                approvals_reviewer: Some(auto_review.approvals_reviewer),
                sandbox_policy: Some(auto_review.sandbox_policy.clone()),
                permission_profile: None,
                windows_sandbox_level: None,
                model: None,
                effort: None,
                summary: None,
                service_tier: None,
                collaboration_mode: None,
                personality: None,
            })
        );

        let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
        let config_value = toml::from_str::<TomlValue>(&config)?;
        let profile_config = config_value
            .as_table()
            .and_then(|table| table.get("profiles"))
            .and_then(TomlValue::as_table)
            .and_then(|profiles| profiles.get("guardian"))
            .and_then(TomlValue::as_table)
            .expect("guardian profile should exist");
        assert_eq!(
            config_value
                .as_table()
                .and_then(|table| table.get("approvals_reviewer")),
            Some(&TomlValue::String("user".to_string()))
        );
        assert_eq!(
            profile_config.get("approvals_reviewer"),
            Some(&TomlValue::String("auto_review".to_string()))
        );
        Ok(())
    }

    #[tokio::test]
    async fn update_feature_flags_disabling_guardian_in_profile_allows_inherited_user_reviewer()
    -> Result<()> {
        let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        app.active_profile = Some("guardian".to_string());
        let config_toml_path = codex_home.path().join("config.toml").abs();
        let config_toml = r#"
profile = "guardian"
approvals_reviewer = "user"

[profiles.guardian]
approvals_reviewer = "guardian_subagent"

[profiles.guardian.features]
guardian_approval = true
"#;
        std::fs::write(config_toml_path.as_path(), config_toml)?;
        let user_config = toml::from_str::<TomlValue>(config_toml)?;
        app.config.config_layer_stack = app
            .config
            .config_layer_stack
            .with_user_config(&config_toml_path, user_config);
        app.config
            .features
            .set_enabled(Feature::GuardianApproval, /*enabled*/ true)?;
        app.chat_widget
            .set_feature_enabled(Feature::GuardianApproval, /*enabled*/ true);
        app.config.approvals_reviewer = ApprovalsReviewer::AutoReview;
        app.chat_widget
            .set_approvals_reviewer(ApprovalsReviewer::AutoReview);

        app.update_feature_flags(vec![(Feature::GuardianApproval, false)])
            .await;

        assert!(!app.config.features.enabled(Feature::GuardianApproval));
        assert!(
            !app.chat_widget
                .config_ref()
                .features
                .enabled(Feature::GuardianApproval)
        );
        assert_eq!(app.config.approvals_reviewer, ApprovalsReviewer::User);
        assert_eq!(
            app.chat_widget.config_ref().approvals_reviewer,
            ApprovalsReviewer::User
        );
        assert_eq!(
            op_rx.try_recv(),
            Ok(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: None,
                approvals_reviewer: Some(ApprovalsReviewer::User),
                sandbox_policy: None,
                permission_profile: None,
                windows_sandbox_level: None,
                model: None,
                effort: None,
                summary: None,
                service_tier: None,
                collaboration_mode: None,
                personality: None,
            })
        );
        let cell = match app_event_rx.try_recv() {
            Ok(AppEvent::InsertHistoryCell(cell)) => cell,
            other => panic!("expected InsertHistoryCell event, got {other:?}"),
        };
        let rendered = cell
            .display_lines(/*width*/ 120)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Permissions updated to Default"));

        let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
        assert!(!config.contains("guardian_approval = true"));
        assert!(!config.contains("guardian_subagent"));
        assert_eq!(
            toml::from_str::<TomlValue>(&config)?
                .as_table()
                .and_then(|table| table.get("approvals_reviewer")),
            Some(&TomlValue::String("user".to_string()))
        );
        Ok(())
    }

    #[tokio::test]
    async fn update_feature_flags_disabling_guardian_in_profile_keeps_inherited_non_user_reviewer_enabled()
    -> Result<()> {
        let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        app.active_profile = Some("guardian".to_string());
        let config_toml_path = codex_home.path().join("config.toml").abs();
        let config_toml = "profile = \"guardian\"\napprovals_reviewer = \"guardian_subagent\"\n\n[features]\nguardian_approval = true\n";
        std::fs::write(config_toml_path.as_path(), config_toml)?;
        let user_config = toml::from_str::<TomlValue>(config_toml)?;
        app.config.config_layer_stack = app
            .config
            .config_layer_stack
            .with_user_config(&config_toml_path, user_config);
        app.config
            .features
            .set_enabled(Feature::GuardianApproval, /*enabled*/ true)?;
        app.chat_widget
            .set_feature_enabled(Feature::GuardianApproval, /*enabled*/ true);
        app.config.approvals_reviewer = ApprovalsReviewer::AutoReview;
        app.chat_widget
            .set_approvals_reviewer(ApprovalsReviewer::AutoReview);

        app.update_feature_flags(vec![(Feature::GuardianApproval, false)])
            .await;

        assert!(app.config.features.enabled(Feature::GuardianApproval));
        assert!(
            app.chat_widget
                .config_ref()
                .features
                .enabled(Feature::GuardianApproval)
        );
        assert_eq!(app.config.approvals_reviewer, ApprovalsReviewer::AutoReview);
        assert_eq!(
            app.chat_widget.config_ref().approvals_reviewer,
            ApprovalsReviewer::AutoReview
        );
        assert!(
            op_rx.try_recv().is_err(),
            "disabling an inherited non-user reviewer should not patch the active session"
        );
        let app_events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            !app_events.iter().any(|event| match event {
                AppEvent::InsertHistoryCell(cell) => cell
                    .display_lines(/*width*/ 120)
                    .iter()
                    .any(|line| line.to_string().contains("Permissions updated to")),
                _ => false,
            }),
            "blocking disable with inherited guardian review should not emit a permissions history update: {app_events:?}"
        );

        let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
        assert!(config.contains("guardian_approval = true"));
        assert_eq!(
            toml::from_str::<TomlValue>(&config)?
                .as_table()
                .and_then(|table| table.get("approvals_reviewer")),
            Some(&TomlValue::String("guardian_subagent".to_string()))
        );
        Ok(())
    }

    #[tokio::test]
    async fn open_agent_picker_allows_existing_agent_threads_when_feature_is_disabled() -> Result<()>
    {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let thread_id = ThreadId::new();
        app.thread_event_channels
            .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));

        app.open_agent_picker(&mut app_server).await;
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::SelectAgentThread(selected_thread_id)) if selected_thread_id == thread_id
        );
        Ok(())
    }

    fn queue_test_oracle_list_threads_result(
        app: &App,
        result: Result<Vec<OracleBrokerThreadEntry>, String>,
    ) {
        app.oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks")
            .list_thread_results
            .push_back(result);
    }

    fn queue_test_oracle_list_threads_delay(app: &App, delay: Duration) {
        app.oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks")
            .list_thread_delay = Some(delay);
    }

    fn queue_test_oracle_new_thread_delay(app: &App, delay: Duration) {
        app.oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks")
            .new_thread_delay = Some(delay);
    }

    fn queue_test_oracle_new_thread_result(
        app: &App,
        result: Result<OracleBrokerThreadOpenResponse, String>,
    ) {
        app.oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks")
            .new_thread_results
            .push_back(result);
    }

    fn queue_test_oracle_attach_thread_delay(app: &App, delay: Duration) {
        app.oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks")
            .attach_thread_delay = Some(delay);
    }

    fn queue_test_oracle_attach_thread_result(
        app: &App,
        result: Result<OracleBrokerThreadOpenResponse, String>,
    ) {
        app.oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks")
            .attach_thread_results
            .push_back(result);
    }

    fn queue_test_oracle_thread_history_result(
        app: &App,
        result: Result<OracleBrokerThreadHistoryResponse, String>,
    ) {
        app.oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks")
            .thread_history_results
            .push_back(result);
    }

    async fn handle_next_oracle_picker_remote_list_result(
        app: &mut App,
        app_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    ) -> Result<()> {
        let (request_id, result) = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match app_event_rx.recv().await {
                    Some(AppEvent::OraclePickerRemoteListTick { request_id }) => {
                        app.handle_oracle_picker_remote_list_tick(request_id);
                    }
                    Some(AppEvent::OraclePickerRemoteThreadsLoaded { request_id, result }) => {
                        return Ok((request_id, result));
                    }
                    Some(_) => {}
                    None => {
                        return Err(color_eyre::eyre::eyre!(
                            "app event channel closed before remote Oracle picker list completed"
                        ));
                    }
                }
            }
        })
        .await
        .map_err(|_| {
            color_eyre::eyre::eyre!("timed out waiting for remote Oracle picker list")
        })??;
        app.handle_oracle_picker_remote_threads_loaded(request_id, result);
        Ok(())
    }

    fn test_oracle_thread_open_response(
        session_id: &str,
        title: &str,
        conversation_id: &str,
    ) -> OracleBrokerThreadOpenResponse {
        OracleBrokerThreadOpenResponse {
            session_id: Some(session_id.to_string()),
            title: title.to_string(),
            conversation_id: Some(conversation_id.to_string()),
            url: Some(format!("https://chatgpt.com/c/{conversation_id}")),
            history: Vec::new(),
        }
    }

    #[tokio::test]
    async fn cached_oracle_broker_match_requires_live_transport() {
        let repo = PathBuf::from("/tmp/oracle");
        let broker = OracleBrokerClient::new_test_client();

        assert!(App::cached_oracle_broker_matches(
            repo.as_path(),
            repo.as_path(),
            &broker,
        ));

        broker.abort();

        assert!(!App::cached_oracle_broker_matches(
            repo.as_path(),
            repo.as_path(),
            &broker,
        ));
    }

    #[tokio::test]
    async fn open_oracle_picker_loads_remote_threads_from_broker_when_idle() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        queue_test_oracle_list_threads_result(
            &app,
            Ok(vec![OracleBrokerThreadEntry {
                title: "Fresh Thread".to_string(),
                conversation_id: "fresh-2".to_string(),
                url: Some("https://chatgpt.com/c/fresh-2".to_string()),
                is_current: false,
            }]),
        );

        app.open_oracle_picker().await?;
        handle_next_oracle_picker_remote_list_result(&mut app, &mut app_event_rx).await?;
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::OracleAttachThread {
                conversation_id,
                title,
                url,
            }) if conversation_id == "fresh-2"
                && title == "Fresh Thread"
                && url.as_deref() == Some("https://chatgpt.com/c/fresh-2")
        );
        assert_eq!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .list_thread_calls,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn open_oracle_picker_releases_browse_only_broker_after_remote_list() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_repo = tempdir()?;
        app.oracle_broker = Some((
            oracle_repo.path().to_path_buf(),
            OracleBrokerClient::new_hanging_test_client(),
        ));
        queue_test_oracle_list_threads_result(
            &app,
            Ok(vec![OracleBrokerThreadEntry {
                title: "Browse Only".to_string(),
                conversation_id: "browse-only".to_string(),
                url: Some("https://chatgpt.com/c/browse-only".to_string()),
                is_current: false,
            }]),
        );

        app.open_oracle_picker().await?;
        handle_next_oracle_picker_remote_list_result(&mut app, &mut app_event_rx).await?;

        assert!(
            app.oracle_broker.is_none(),
            "browse-only picker should not keep the Oracle broker/browser alive"
        );
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::OracleAttachThread {
                conversation_id,
                ..
            }) if conversation_id == "browse-only"
        );
        Ok(())
    }

    #[tokio::test]
    async fn startup_oracle_picker_action_opens_picker_after_primary_session_attach() -> Result<()>
    {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        app.pending_startup_ui_action = Some(StartupUiAction::OpenOraclePicker);

        app.enqueue_primary_thread_session(
            test_thread_session(ThreadId::new(), PathBuf::from("/tmp/project")),
            Vec::new(),
        )
        .await?;

        assert!(
            app.chat_widget
                .selection_view_is_active(ORACLE_SELECTION_VIEW_ID)
        );
        assert_eq!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .list_thread_calls,
            0
        );

        let popup = render_bottom_popup(&app, /*width*/ 100);
        assert!(popup.contains("Oracle"));
        Ok(())
    }

    #[tokio::test]
    async fn show_oracle_picker_dedupes_attached_remote_threads_and_emits_attach_event() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let attached_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            attached_thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        app.activate_oracle_binding(attached_thread_id);
        app.sync_oracle_thread_session(attached_thread_id).await;

        app.show_oracle_picker(
            vec![
                OracleBrokerThreadEntry {
                    title: "Attached Thread".to_string(),
                    conversation_id: "attached-1".to_string(),
                    url: Some("https://chatgpt.com/c/attached-1".to_string()),
                    is_current: false,
                },
                OracleBrokerThreadEntry {
                    title: "Fresh Thread".to_string(),
                    conversation_id: "fresh-2".to_string(),
                    url: Some("https://chatgpt.com/c/fresh-2".to_string()),
                    is_current: false,
                },
            ],
            /*include_new_thread*/ true,
        );

        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::OracleAttachThread {
                conversation_id,
                title,
                url,
            }) if conversation_id == "fresh-2"
                && title == "Fresh Thread"
                && url.as_deref() == Some("https://chatgpt.com/c/fresh-2")
        );
    }

    #[tokio::test]
    async fn show_oracle_picker_keeps_actions_out_of_thread_list_and_surfaces_hotkeys() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let attached_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            attached_thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        app.activate_oracle_binding(attached_thread_id);
        app.sync_oracle_thread_session(attached_thread_id).await;

        app.show_oracle_picker(
            vec![OracleBrokerThreadEntry {
                title: "Fresh Thread".to_string(),
                conversation_id: "fresh-2".to_string(),
                url: Some("https://chatgpt.com/c/fresh-2".to_string()),
                is_current: false,
            }],
            /*include_new_thread*/ true,
        );

        let popup = render_bottom_popup(&app, /*width*/ 100);

        assert!(!popup.contains("Action:"));
        assert!(popup.contains("Hotkeys:"));
        assert!(popup.contains(" new"));
        assert!(popup.contains(" search"));
        assert!(popup.contains("info"));
        assert!(popup.contains("thinking 5.5"));
        assert!(!popup.contains("https://chatgpt.com/"));
        assert!(!popup.contains("session "));
        assert!(!popup.contains("convo "));
        assert!(!popup.contains("requested gpt-5.5"));
        assert!(popup.contains("Attached: Attached Thread"));
        assert!(popup.contains("Remote: Fresh Thread"));
    }

    #[tokio::test]
    async fn show_oracle_picker_keeps_remote_row_when_duplicate_local_bindings_exist() {
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let first_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            first_thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread A".to_string()),
        );
        let second_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            second_thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread B".to_string()),
        );

        app.show_oracle_picker(
            vec![OracleBrokerThreadEntry {
                title: "Attached Thread".to_string(),
                conversation_id: "attached-1".to_string(),
                url: Some("https://chatgpt.com/c/attached-1".to_string()),
                is_current: false,
            }],
            /*include_new_thread*/ false,
        );

        let popup = render_bottom_popup(&app, /*width*/ 100);
        assert!(popup.contains("Attached: Attached Thread A"));
        assert!(popup.contains("Attached: Attached Thread B"));
        assert!(popup.contains("Remote: Attached Thread"));
    }

    #[tokio::test]
    async fn oracle_workflow_picker_lists_supervisor_and_hidden_orchestrator() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.persist_active_oracle_binding();

        app.open_oracle_workflow_picker(oracle_thread_id);

        let popup = render_bottom_popup(&app, /*width*/ 100);
        assert!(popup.contains("Oracle Workflow"));
        assert!(popup.contains("Oracle [supervisor]"));
        assert!(popup.contains("Oracle Orchestrator [orchestrator]"));

        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::SelectAgentThread(selected_thread_id))
                if selected_thread_id == orchestrator_thread_id
        );
        Ok(())
    }

    #[tokio::test]
    async fn open_oracle_picker_shows_local_threads_only_while_oracle_browser_is_busy() -> Result<()>
    {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.activate_oracle_binding(thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.persist_active_oracle_binding();

        app.open_oracle_picker().await?;
        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::InsertHistoryCell(cell))
                if cell
                    .display_lines(/*width*/ 120)
                    .iter()
                    .any(|line| line.to_string().contains("showing attached local Oracle threads only"))
        );
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::SelectAgentThread(selected_thread_id)) if selected_thread_id == thread_id
        );
        Ok(())
    }

    #[tokio::test]
    async fn create_oracle_thread_binding_updates_local_state_from_remote_metadata() -> Result<()> {
        let mut app = make_test_app().await;
        let root_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session(
            root_thread_id,
            Some("runtime-root".to_string()),
            true,
        );
        queue_test_oracle_new_thread_result(
            &app,
            Ok(OracleBrokerThreadOpenResponse {
                session_id: Some("runtime-3".to_string()),
                title: "Fresh Thread".to_string(),
                conversation_id: Some("fresh-3".to_string()),
                url: Some("https://chatgpt.com/c/fresh-3".to_string()),
                history: Vec::new(),
            }),
        );

        let binding = app.create_oracle_thread_binding().await?;

        assert_eq!(binding.title, "Fresh Thread");
        assert!(binding.attached_remote);
        assert_eq!(app.oracle_state.oracle_thread_id, Some(binding.thread_id));
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&binding.thread_id)
                .and_then(|binding| binding.current_session_id.as_deref()),
            Some("runtime-3")
        );
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&binding.thread_id)
                .and_then(|binding| binding.conversation_id.as_deref()),
            Some("fresh-3")
        );
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&binding.thread_id)
                .and_then(|binding| binding.remote_title.as_deref()),
            Some("Fresh Thread")
        );
        assert_eq!(
            app.oracle_state.last_status.as_deref(),
            Some(format!("Attached new Oracle thread on {}.", binding.thread_id).as_str())
        );
        assert_eq!(
            app.agent_navigation
                .get(&binding.thread_id)
                .and_then(|entry| entry.agent_role.as_deref()),
            Some("supervisor")
        );
        let channel = app
            .thread_event_channels
            .get(&binding.thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert_eq!(
            store.session.as_ref().map(|session| session.model.as_str()),
            Some("requested gpt-5.5-pro (Standard)")
        );
        assert_eq!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .new_thread_calls,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn preferred_oracle_followup_session_stays_anchor_scoped() {
        let mut app = make_test_app().await;
        let attached_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session_id(
            attached_thread_id,
            Some("runtime-root".to_string()),
        );
        let unattached_thread_id = app.create_visible_oracle_thread().await;
        app.activate_oracle_binding(unattached_thread_id);

        assert_eq!(
            app.oracle_state.oracle_thread_id,
            Some(unattached_thread_id)
        );
        assert_eq!(app.preferred_oracle_followup_session(), None);
    }

    #[tokio::test]
    async fn preferred_oracle_followup_session_requires_verified_broker_session() {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session_id(thread_id, Some("runtime-root".to_string()));
        app.activate_oracle_binding(thread_id);

        assert_eq!(app.preferred_oracle_followup_session(), None);

        app.set_oracle_thread_broker_session(thread_id, Some("runtime-root".to_string()), true);
        assert_eq!(
            app.preferred_oracle_followup_session().as_deref(),
            Some("runtime-root")
        );
    }

    #[tokio::test]
    async fn create_oracle_thread_binding_does_not_borrow_another_binding_session() -> Result<()> {
        let mut app = make_test_app().await;
        let attached_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session_id(
            attached_thread_id,
            Some("runtime-root".to_string()),
        );
        let unattached_thread_id = app.create_visible_oracle_thread().await;
        app.activate_oracle_binding(unattached_thread_id);
        queue_test_oracle_new_thread_result(
            &app,
            Ok(OracleBrokerThreadOpenResponse {
                session_id: Some("runtime-fallback".to_string()),
                title: "Fallback Thread".to_string(),
                conversation_id: Some("fallback-1".to_string()),
                url: Some("https://chatgpt.com/c/fallback-1".to_string()),
                history: Vec::new(),
            }),
        );

        let binding = app.create_oracle_thread_binding().await?;
        let hooks = app
            .oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks");

        assert!(!binding.attached_remote);
        assert_eq!(hooks.new_thread_calls, 0);
        assert!(hooks.new_thread_followup_sessions.is_empty());
        assert_ne!(binding.thread_id, attached_thread_id);
        assert_ne!(binding.thread_id, unattached_thread_id);
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&binding.thread_id)
                .and_then(|binding| binding.current_session_id.as_deref()),
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn create_oracle_thread_binding_times_out_remote_thread_creation() {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session(thread_id, Some("runtime-root".to_string()), true);
        app.activate_oracle_binding(thread_id);
        queue_test_oracle_new_thread_delay(&app, Duration::from_millis(75));

        let err = app
            .create_oracle_thread_binding()
            .await
            .expect_err("slow remote thread creation should time out");

        assert_eq!(
            err.to_string(),
            "Timed out after 50 ms while creating a remote Oracle thread."
        );
        assert_eq!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .new_thread_calls,
            0
        );
    }

    #[tokio::test]
    async fn create_oracle_thread_binding_preserves_blank_broker_session_for_first_turn()
    -> Result<()> {
        let mut app = make_test_app().await;
        let root_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session(
            root_thread_id,
            Some("runtime-root".to_string()),
            true,
        );
        queue_test_oracle_new_thread_result(
            &app,
            Ok(OracleBrokerThreadOpenResponse {
                session_id: Some("runtime-blank".to_string()),
                title: "Blank Thread".to_string(),
                conversation_id: None,
                url: Some("https://chatgpt.com/".to_string()),
                history: Vec::new(),
            }),
        );

        let binding = app.create_oracle_thread_binding().await?;
        let thread_binding = app
            .oracle_state
            .bindings
            .get(&binding.thread_id)
            .expect("oracle binding");
        assert_eq!(
            thread_binding.current_session_id.as_deref(),
            Some("runtime-blank")
        );
        assert_eq!(
            thread_binding.current_session_ownership,
            Some(OracleSessionOwnership::BrokerThread)
        );
        assert_eq!(
            app.preferred_oracle_followup_session().as_deref(),
            Some("runtime-blank")
        );
        Ok(())
    }

    #[tokio::test]
    async fn create_oracle_thread_binding_bootstraps_local_thread_before_first_browser_turn()
    -> Result<()> {
        let mut app = make_test_app().await;

        let binding = app.create_oracle_thread_binding().await?;

        assert_eq!(binding.title, "Oracle");
        assert!(!binding.attached_remote);
        assert_eq!(app.oracle_state.oracle_thread_id, Some(binding.thread_id));
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&binding.thread_id)
                .and_then(|binding| binding.current_session_id.as_deref()),
            None
        );
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&binding.thread_id)
                .and_then(|binding| binding.conversation_id.as_deref()),
            None
        );
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&binding.thread_id)
                .and_then(|binding| binding.remote_title.as_deref()),
            None
        );
        assert_eq!(
            app.oracle_state.last_status.as_deref(),
            Some(
                format!(
                    "Created new Oracle thread on {}. The hidden Oracle browser session will attach on the first turn.",
                    binding.thread_id
                )
                .as_str()
            )
        );
        assert_eq!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .new_thread_calls,
            0
        );
        Ok(())
    }

    #[tokio::test]
    async fn create_oracle_thread_binding_does_not_reuse_unowned_stale_followup_session()
    -> Result<()> {
        let mut app = make_test_app().await;
        app.oracle_state.current_session_id = Some("oracle-hidden-stale".to_string());

        let binding = app.create_oracle_thread_binding().await?;

        let hooks = app
            .oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks");
        assert!(!binding.attached_remote);
        assert_eq!(hooks.new_thread_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn create_oracle_thread_binding_does_not_reuse_mismatched_legacy_run_session()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("remote-stale".to_string()),
            Some("Stale Thread".to_string()),
        );
        {
            let binding = app
                .oracle_state
                .bindings
                .get_mut(&thread_id)
                .expect("oracle binding");
            binding.session_root_slug = Some("oracle-thread-1".to_string());
            binding.current_session_id = Some("manual-check-codex-oracle-handoff".to_string());
        }
        app.activate_oracle_binding(thread_id);

        let binding = app.create_oracle_thread_binding().await?;

        let hooks = app
            .oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks");
        assert!(!binding.attached_remote);
        assert_ne!(binding.thread_id, thread_id);
        assert_eq!(hooks.new_thread_calls, 0);
        Ok(())
    }

    #[tokio::test]
    async fn create_oracle_thread_binding_rejects_busy_browser_state() {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.activate_oracle_binding(thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.persist_active_oracle_binding();

        let err = app
            .create_oracle_thread_binding()
            .await
            .expect_err("busy oracle browser should block remote thread mutation");

        assert_eq!(
            err.to_string(),
            app.oracle_remote_thread_mutation_blocked_message()
        );
        assert_eq!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .new_thread_calls,
            0
        );
    }

    #[tokio::test]
    async fn attach_oracle_thread_binding_reuses_existing_verified_local_thread_without_remote_attach()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session(thread_id, Some("runtime-root".to_string()), true);
        app.set_oracle_remote_metadata(
            thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        app.sync_oracle_thread_session(thread_id).await;

        let binding = app
            .attach_oracle_thread_binding(
                "attached-1".to_string(),
                None,
                Some("Attached Thread".to_string()),
            )
            .await?;

        assert_eq!(
            binding,
            OracleAttachThreadBinding::ReattachedLocal {
                thread_id,
                title: "Attached Thread".to_string(),
                conversation_id: "attached-1".to_string(),
            }
        );
        assert_eq!(app.oracle_state.oracle_thread_id, Some(thread_id));
        assert_eq!(
            app.oracle_state.last_status.as_deref(),
            Some(format!("Reattached existing local Oracle thread {thread_id}.").as_str())
        );
        assert!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .attach_thread_calls
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn attach_oracle_thread_binding_rebinds_existing_broker_thread_session_before_reuse()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session_id(thread_id, Some("runtime-root".to_string()));
        app.set_oracle_remote_metadata(
            thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        queue_test_oracle_attach_thread_result(
            &app,
            Ok(test_oracle_thread_open_response(
                "runtime-verified",
                "Attached Thread",
                "attached-1",
            )),
        );

        let binding = app
            .attach_oracle_thread_binding(
                "attached-1".to_string(),
                None,
                Some("Attached Thread".to_string()),
            )
            .await?;

        assert_eq!(
            binding,
            OracleAttachThreadBinding::AttachedRemote {
                thread_id,
                title: "Attached Thread".to_string(),
                conversation_id: "attached-1".to_string(),
                history: Vec::new(),
            }
        );
        let hooks = app
            .oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks");
        assert_eq!(hooks.attach_thread_calls, vec!["attached-1".to_string()]);
        assert_eq!(hooks.attach_thread_followup_sessions, vec![None]);
        drop(hooks);
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&thread_id)
                .and_then(|binding| binding.current_session_id.as_deref()),
            Some("runtime-verified")
        );
        Ok(())
    }

    #[tokio::test]
    async fn oracle_attach_command_with_import_history_reuses_existing_local_thread_and_fetches_history()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session(thread_id, Some("runtime-root".to_string()), true);
        app.set_oracle_remote_metadata(
            thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        app.sync_oracle_thread_session(thread_id).await;
        queue_test_oracle_thread_history_result(
            &app,
            Ok(OracleBrokerThreadHistoryResponse {
                session_id: Some("runtime-root".to_string()),
                title: "Attached Thread".to_string(),
                conversation_id: Some("attached-1".to_string()),
                url: Some("https://chatgpt.com/c/attached-1".to_string()),
                history_window: None,
                history: vec![
                    OracleBrokerThreadHistoryEntry {
                        role: "user".to_string(),
                        text: "Earlier question".to_string(),
                    },
                    OracleBrokerThreadHistoryEntry {
                        role: "assistant".to_string(),
                        text: "Earlier answer".to_string(),
                    },
                ],
            }),
        );

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_configure_oracle_command(
            None,
            &mut app_server,
            "attach attached-1 --import-history",
        )
        .await?;

        {
            let hooks = app
                .oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks");
            assert!(hooks.attach_thread_calls.is_empty());
            assert_eq!(
                hooks.thread_history_followup_sessions,
                vec![Some("runtime-root".to_string())]
            );
        }

        assert_eq!(app.oracle_state.oracle_thread_id, Some(thread_id));
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert_eq!(store.turns.len(), 1);
        assert!(matches!(
            store.turns[0].items.as_slice(),
            [
                ThreadItem::UserMessage { .. },
                ThreadItem::AgentMessage { text, .. }
            ] if text == "Earlier answer"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn attach_oracle_thread_binding_reattaches_remote_when_local_binding_lost_session()
    -> Result<()> {
        let mut app = make_test_app().await;
        let usable_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session_id(usable_thread_id, Some("runtime-root".to_string()));
        let stale_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            stale_thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        queue_test_oracle_attach_thread_result(
            &app,
            Ok(test_oracle_thread_open_response(
                "runtime-reattached",
                "Attached Thread",
                "attached-1",
            )),
        );

        let binding = app
            .attach_oracle_thread_binding(
                "attached-1".to_string(),
                None,
                Some("Attached Thread".to_string()),
            )
            .await?;
        let hooks = app
            .oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks");

        assert_eq!(
            binding,
            OracleAttachThreadBinding::AttachedRemote {
                thread_id: stale_thread_id,
                title: "Attached Thread".to_string(),
                conversation_id: "attached-1".to_string(),
                history: Vec::new(),
            }
        );
        assert_eq!(hooks.attach_thread_calls, vec!["attached-1".to_string()]);
        assert_eq!(hooks.attach_thread_followup_sessions, vec![None]);
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&stale_thread_id)
                .and_then(|binding| binding.current_session_id.as_deref()),
            Some("runtime-reattached")
        );
        Ok(())
    }

    #[tokio::test]
    async fn attach_oracle_thread_binding_fetches_remote_thread_when_not_already_bound()
    -> Result<()> {
        let mut app = make_test_app().await;
        queue_test_oracle_attach_thread_result(
            &app,
            Ok(test_oracle_thread_open_response(
                "runtime-7",
                "Remote Thread",
                "remote-7",
            )),
        );

        let binding = app
            .attach_oracle_thread_binding("remote-7".to_string(), None, None)
            .await?;
        let thread_id = binding.thread_id();

        assert_eq!(
            binding,
            OracleAttachThreadBinding::AttachedRemote {
                thread_id,
                title: "Remote Thread".to_string(),
                conversation_id: "remote-7".to_string(),
                history: Vec::new(),
            }
        );
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&thread_id)
                .and_then(|binding| binding.current_session_id.as_deref()),
            Some("runtime-7")
        );
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&thread_id)
                .and_then(|binding| binding.conversation_id.as_deref()),
            Some("remote-7")
        );
        assert_eq!(
            app.oracle_state.last_status.as_deref(),
            Some(format!("Attached Oracle thread on {thread_id}.").as_str())
        );
        assert_eq!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .attach_thread_calls,
            vec!["remote-7".to_string()]
        );
        assert_eq!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .attach_thread_followup_sessions,
            vec![None]
        );
        Ok(())
    }

    #[tokio::test]
    async fn attach_oracle_thread_binding_rejects_mismatched_remote_conversation_response()
    -> Result<()> {
        let mut app = make_test_app().await;
        queue_test_oracle_attach_thread_result(
            &app,
            Ok(test_oracle_thread_open_response(
                "runtime-7",
                "Remote Thread",
                "wrong-9",
            )),
        );

        let err = app
            .attach_oracle_thread_binding("remote-7".to_string(), None, None)
            .await
            .expect_err("mismatched conversation id should fail");

        assert!(err.to_string().contains("wrong-9"));
        assert!(app.oracle_state.bindings.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn attach_oracle_thread_binding_rejects_mismatched_remote_thread_url_scope() -> Result<()>
    {
        let mut app = make_test_app().await;
        queue_test_oracle_attach_thread_result(
            &app,
            Ok(OracleBrokerThreadOpenResponse {
                url: Some("https://chatgpt.com/g/oracle-project/c/remote-7".to_string()),
                ..test_oracle_thread_open_response("runtime-7", "Remote Thread", "remote-7")
            }),
        );

        let err = app
            .attach_oracle_thread_binding(
                "remote-7".to_string(),
                Some("https://chatgpt.com/c/remote-7".to_string()),
                None,
            )
            .await
            .expect_err("mismatched thread scope should fail closed");

        assert!(err.to_string().contains("URL/scope"));
        assert!(app.oracle_state.bindings.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn attach_oracle_thread_binding_restores_previous_active_binding_when_remote_attach_fails()
     {
        let mut app = make_test_app().await;
        let previous_thread_id = app.create_visible_oracle_thread().await;
        let attached_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            attached_thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        app.activate_oracle_binding(previous_thread_id);
        app.oracle_state.last_status = Some("Before attach".to_string());
        app.persist_active_oracle_binding();
        queue_test_oracle_attach_thread_delay(&app, Duration::from_millis(75));

        let err = app
            .attach_oracle_thread_binding(
                "attached-1".to_string(),
                None,
                Some("Attached Thread".to_string()),
            )
            .await
            .expect_err("slow remote attach should time out");

        assert_eq!(
            err.to_string(),
            "Timed out after 50 ms while attaching a remote Oracle thread."
        );
        assert_eq!(app.oracle_state.oracle_thread_id, Some(previous_thread_id));
        assert_eq!(
            app.oracle_state.last_status.as_deref(),
            Some("Before attach")
        );
    }

    #[tokio::test]
    async fn oracle_attach_command_with_import_history_ignores_unbounded_inline_attach_history()
    -> Result<()> {
        let mut app = make_test_app().await;
        let inline_attach_history = vec![
            OracleBrokerThreadHistoryEntry {
                role: "user".to_string(),
                text: "Earlier question".to_string(),
            },
            OracleBrokerThreadHistoryEntry {
                role: "assistant".to_string(),
                text: "Earlier answer".to_string(),
            },
            OracleBrokerThreadHistoryEntry {
                role: "user".to_string(),
                text: "Later question".to_string(),
            },
        ];
        let fetched_history = vec![
            OracleBrokerThreadHistoryEntry {
                role: "user".to_string(),
                text: "Earlier question".to_string(),
            },
            OracleBrokerThreadHistoryEntry {
                role: "assistant".to_string(),
                text: "Earlier answer".to_string(),
            },
        ];
        queue_test_oracle_attach_thread_result(
            &app,
            Ok(OracleBrokerThreadOpenResponse {
                history: inline_attach_history,
                ..test_oracle_thread_open_response("runtime-7", "Remote Thread", "remote-7")
            }),
        );
        queue_test_oracle_thread_history_result(
            &app,
            Ok(OracleBrokerThreadHistoryResponse {
                session_id: Some("runtime-7".to_string()),
                title: "Remote Thread".to_string(),
                conversation_id: Some("remote-7".to_string()),
                url: Some("https://chatgpt.com/c/remote-7".to_string()),
                history_window: Some(OracleBrokerThreadHistoryWindow {
                    limit: 2,
                    returned_count: 2,
                    total_count: 3,
                    truncated: true,
                }),
                history: fetched_history,
            }),
        );

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_configure_oracle_command(
            None,
            &mut app_server,
            "attach remote-7 --import-history",
        )
        .await?;

        let thread_id = app.oracle_state.oracle_thread_id.expect("oracle thread");
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(binding.conversation_id.as_deref(), Some("remote-7"));
        {
            let hooks = app
                .oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks");
            assert_eq!(hooks.attach_thread_calls, vec!["remote-7".to_string()]);
            assert_eq!(
                hooks.thread_history_followup_sessions,
                vec![Some("runtime-7".to_string())]
            );
        }

        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert_eq!(store.turns.len(), 1);
        assert!(matches!(
            store.turns[0].items.as_slice(),
            [
                ThreadItem::UserMessage { .. },
                ThreadItem::AgentMessage { text, .. }
            ] if text == "Earlier answer"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn oracle_attach_command_with_import_history_falls_back_to_thread_history_fetch()
    -> Result<()> {
        let mut app = make_test_app().await;
        queue_test_oracle_attach_thread_result(
            &app,
            Ok(test_oracle_thread_open_response(
                "runtime-7",
                "Remote Thread",
                "remote-7",
            )),
        );
        queue_test_oracle_thread_history_result(
            &app,
            Ok(OracleBrokerThreadHistoryResponse {
                session_id: Some("runtime-7".to_string()),
                title: "Remote Thread".to_string(),
                conversation_id: Some("remote-7".to_string()),
                url: Some("https://chatgpt.com/c/remote-7".to_string()),
                history_window: None,
                history: vec![OracleBrokerThreadHistoryEntry {
                    role: "assistant".to_string(),
                    text: "Imported from fallback".to_string(),
                }],
            }),
        );

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_configure_oracle_command(
            None,
            &mut app_server,
            "attach remote-7 --import-history",
        )
        .await?;

        let thread_id = app.oracle_state.oracle_thread_id.expect("oracle thread");
        {
            let hooks = app
                .oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks");
            assert_eq!(hooks.attach_thread_calls, vec!["remote-7".to_string()]);
            assert_eq!(
                hooks.thread_history_followup_sessions,
                vec![Some("runtime-7".to_string())]
            );
        }
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert!(matches!(
            store.turns[0].items.as_slice(),
            [ThreadItem::AgentMessage { text, .. }] if text == "Imported from fallback"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn submit_thread_op_routes_attached_import_history_oracle_thread_through_supervisor()
    -> Result<()> {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        std::fs::create_dir_all(app.chat_widget.config_ref().cwd.as_path())?;
        let oracle_repo =
            find_oracle_repo(app.chat_widget.config_ref().cwd.as_path()).expect("oracle repo");
        app.oracle_broker = Some((oracle_repo, OracleBrokerClient::new_test_client()));
        queue_test_oracle_attach_thread_result(
            &app,
            Ok(test_oracle_thread_open_response(
                "runtime-7",
                "Remote Thread",
                "remote-7",
            )),
        );
        queue_test_oracle_thread_history_result(
            &app,
            Ok(OracleBrokerThreadHistoryResponse {
                session_id: Some("runtime-7".to_string()),
                title: "Remote Thread".to_string(),
                conversation_id: Some("remote-7".to_string()),
                url: Some("https://chatgpt.com/c/remote-7".to_string()),
                history_window: None,
                history: vec![OracleBrokerThreadHistoryEntry {
                    role: "assistant".to_string(),
                    text: "Imported from fallback".to_string(),
                }],
            }),
        );

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_configure_oracle_command(
            None,
            &mut app_server,
            "attach remote-7 --import-history",
        )
        .await?;
        let thread_id = app.oracle_state.oracle_thread_id.expect("oracle thread");
        {
            let hooks = app
                .oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks");
            assert_eq!(hooks.attach_thread_calls, vec!["remote-7".to_string()]);
            assert_eq!(
                hooks.thread_history_followup_sessions,
                vec![Some("runtime-7".to_string())]
            );
        }
        while op_rx.try_recv().is_ok() {}

        app.submit_thread_op(
            &mut app_server,
            thread_id,
            AppCommand::user_turn(
                vec![UserInput::Text {
                    text: "Reply with exactly routed-through-oracle and nothing else.".to_string(),
                    text_elements: Vec::new(),
                }],
                app.chat_widget.config_ref().cwd.to_path_buf(),
                app.chat_widget
                    .config_ref()
                    .permissions
                    .approval_policy
                    .value(),
                app.chat_widget.config_ref().legacy_sandbox_policy(),
                Some(
                    app.chat_widget
                        .config_ref()
                        .permissions
                        .permission_profile(),
                ),
                app.oracle_thread_session(thread_id).model,
                None,
                None,
                app.chat_widget.config_ref().service_tier.map(Some),
                None,
                None,
                app.chat_widget.config_ref().personality,
            ),
        )
        .await?;

        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn)
        );
        assert!(app.oracle_state.active_run_id.is_some());
        assert!(app.oracle_state.pending_turn_id.is_some());
        let request = app
            .oracle_state
            .inflight_run_requests
            .values()
            .next()
            .expect("oracle request");
        assert_eq!(request.oracle_thread_id, thread_id);
        assert_eq!(request.kind, OracleRequestKind::UserTurn);
        while let Ok(op) = op_rx.try_recv() {
            assert!(
                !matches!(op, Op::UserTurn { .. }),
                "oracle attached thread should not emit app-server UserTurn ops",
            );
        }

        app.shutdown_oracle_broker(true);
        Ok(())
    }

    #[tokio::test]
    async fn oracle_fetch_remote_thread_history_rejects_mismatched_conversation_response()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session_id(thread_id, Some("runtime-7".to_string()));
        app.set_oracle_remote_metadata(
            thread_id,
            Some("remote-7".to_string()),
            Some("Remote Thread".to_string()),
        );
        queue_test_oracle_thread_history_result(
            &app,
            Ok(OracleBrokerThreadHistoryResponse {
                session_id: Some("runtime-7".to_string()),
                title: "Wrong Thread".to_string(),
                conversation_id: Some("wrong-9".to_string()),
                url: Some("https://chatgpt.com/c/wrong-9".to_string()),
                history_window: None,
                history: Vec::new(),
            }),
        );

        let err = app
            .oracle_fetch_remote_thread_history(thread_id)
            .await
            .expect_err("mismatched conversation id should fail");

        assert!(err.to_string().contains("wrong-9"));
        Ok(())
    }

    #[tokio::test]
    async fn import_oracle_thread_history_appends_only_missing_suffix_without_duplication()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("remote-7".to_string()),
            Some("Remote Thread".to_string()),
        );
        app.push_oracle_turn(
            thread_id,
            Turn {
                id: "local-turn-1".to_string(),
                items: vec![
                    ThreadItem::UserMessage {
                        id: "user-1".to_string(),
                        content: vec![codex_app_server_protocol::UserInput::Text {
                            text: "Earlier question".to_string(),
                            text_elements: Vec::new(),
                        }],
                    },
                    ThreadItem::AgentMessage {
                        id: "assistant-1".to_string(),
                        text: "Earlier answer".to_string(),
                        phase: None,
                        memory_citation: None,
                    },
                ],
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            },
        )
        .await;

        let outcome = app
            .import_oracle_thread_history(
                None,
                thread_id,
                "remote-7",
                vec![
                    OracleBrokerThreadHistoryEntry {
                        role: "user".to_string(),
                        text: "Earlier question".to_string(),
                    },
                    OracleBrokerThreadHistoryEntry {
                        role: "assistant".to_string(),
                        text: "Earlier answer".to_string(),
                    },
                    OracleBrokerThreadHistoryEntry {
                        role: "user".to_string(),
                        text: "Later question".to_string(),
                    },
                ],
            )
            .await?;
        assert_eq!(
            outcome,
            OracleHistoryImportOutcome::Imported { message_count: 1 }
        );

        let outcome = app
            .import_oracle_thread_history(
                None,
                thread_id,
                "remote-7",
                vec![
                    OracleBrokerThreadHistoryEntry {
                        role: "user".to_string(),
                        text: "Earlier question".to_string(),
                    },
                    OracleBrokerThreadHistoryEntry {
                        role: "assistant".to_string(),
                        text: "Earlier answer".to_string(),
                    },
                    OracleBrokerThreadHistoryEntry {
                        role: "user".to_string(),
                        text: "Later question".to_string(),
                    },
                ],
            )
            .await?;
        assert_eq!(outcome, OracleHistoryImportOutcome::NoNewMessages);

        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert_eq!(store.turns.len(), 2);
        assert!(matches!(
            store.turns[1].items.as_slice(),
            [ThreadItem::UserMessage { .. }]
        ));
        Ok(())
    }

    #[tokio::test]
    async fn import_oracle_thread_history_accepts_bounded_remote_history_windows() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("remote-7".to_string()),
            Some("Remote Thread".to_string()),
        );
        app.push_oracle_turn(
            thread_id,
            Turn {
                id: "local-turn-1".to_string(),
                items: vec![
                    ThreadItem::UserMessage {
                        id: "user-1".to_string(),
                        content: vec![codex_app_server_protocol::UserInput::Text {
                            text: "Earlier question".to_string(),
                            text_elements: Vec::new(),
                        }],
                    },
                    ThreadItem::AgentMessage {
                        id: "assistant-1".to_string(),
                        text: "Earlier answer".to_string(),
                        phase: None,
                        memory_citation: None,
                    },
                ],
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            },
        )
        .await;
        app.push_oracle_turn(
            thread_id,
            Turn {
                id: "local-turn-2".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "user-2".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "Later question".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            },
        )
        .await;

        let outcome = app
            .import_oracle_thread_history(
                None,
                thread_id,
                "remote-7",
                vec![
                    OracleBrokerThreadHistoryEntry {
                        role: "assistant".to_string(),
                        text: "Earlier answer".to_string(),
                    },
                    OracleBrokerThreadHistoryEntry {
                        role: "user".to_string(),
                        text: "Later question".to_string(),
                    },
                    OracleBrokerThreadHistoryEntry {
                        role: "assistant".to_string(),
                        text: "New remote answer".to_string(),
                    },
                ],
            )
            .await?;

        assert_eq!(
            outcome,
            OracleHistoryImportOutcome::Imported { message_count: 1 }
        );
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert_eq!(store.turns.len(), 2);
        assert!(matches!(
            store.turns[1].items.as_slice(),
            [
                ThreadItem::UserMessage { .. },
                ThreadItem::AgentMessage { text, .. }
            ] if text == "New remote answer"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn import_oracle_thread_history_accepts_single_user_overlap_for_pending_local_turn()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("remote-7".to_string()),
            Some("Remote Thread".to_string()),
        );
        app.push_oracle_turn(
            thread_id,
            Turn {
                id: "local-turn-1".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "user-1".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "Later question".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            },
        )
        .await;

        let outcome = app
            .import_oracle_thread_history(
                None,
                thread_id,
                "remote-7",
                vec![
                    OracleBrokerThreadHistoryEntry {
                        role: "user".to_string(),
                        text: "Later question".to_string(),
                    },
                    OracleBrokerThreadHistoryEntry {
                        role: "assistant".to_string(),
                        text: "New remote answer".to_string(),
                    },
                ],
            )
            .await?;

        assert_eq!(
            outcome,
            OracleHistoryImportOutcome::Imported { message_count: 1 }
        );
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert_eq!(store.turns.len(), 1);
        assert!(matches!(
            store.turns[0].items.as_slice(),
            [
                ThreadItem::UserMessage { .. },
                ThreadItem::AgentMessage { text, .. }
            ] if text == "New remote answer"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn import_oracle_thread_history_rejects_single_message_overlap_on_long_diverged_history()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("remote-7".to_string()),
            Some("Remote Thread".to_string()),
        );
        app.push_oracle_turn(
            thread_id,
            Turn {
                id: "local-turn-1".to_string(),
                items: vec![
                    ThreadItem::UserMessage {
                        id: "user-1".to_string(),
                        content: vec![codex_app_server_protocol::UserInput::Text {
                            text: "Different question".to_string(),
                            text_elements: Vec::new(),
                        }],
                    },
                    ThreadItem::AgentMessage {
                        id: "assistant-1".to_string(),
                        text: "Different answer".to_string(),
                        phase: None,
                        memory_citation: None,
                    },
                ],
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            },
        )
        .await;
        app.push_oracle_turn(
            thread_id,
            Turn {
                id: "local-turn-2".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "user-2".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "ok".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            },
        )
        .await;

        let outcome = app
            .import_oracle_thread_history(
                None,
                thread_id,
                "remote-7",
                vec![
                    OracleBrokerThreadHistoryEntry {
                        role: "user".to_string(),
                        text: "ok".to_string(),
                    },
                    OracleBrokerThreadHistoryEntry {
                        role: "assistant".to_string(),
                        text: "Wrong remote answer".to_string(),
                    },
                ],
            )
            .await?;

        assert_eq!(outcome, OracleHistoryImportOutcome::SkippedToAvoidConflict);
        Ok(())
    }

    #[tokio::test]
    async fn import_oracle_thread_history_skips_diverged_local_transcript() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.push_oracle_turn(
            thread_id,
            Turn {
                id: "local-turn-1".to_string(),
                items: vec![ThreadItem::AgentMessage {
                    id: "assistant-1".to_string(),
                    text: "Local only reply".to_string(),
                    phase: None,
                    memory_citation: None,
                }],
                status: TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            },
        )
        .await;

        let outcome = app
            .import_oracle_thread_history(
                None,
                thread_id,
                "remote-7",
                vec![OracleBrokerThreadHistoryEntry {
                    role: "assistant".to_string(),
                    text: "Different remote reply".to_string(),
                }],
            )
            .await?;

        assert_eq!(outcome, OracleHistoryImportOutcome::SkippedToAvoidConflict);
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert_eq!(store.turns.len(), 1);
        assert!(matches!(
            store.turns[0].items.as_slice(),
            [ThreadItem::AgentMessage { text, .. }] if text == "Local only reply"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn attach_oracle_thread_binding_rejects_duplicate_local_conversation_bindings()
    -> Result<()> {
        let mut app = make_test_app().await;
        let first_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            first_thread_id,
            Some("attached-1".to_string()),
            Some("Thread A".to_string()),
        );
        let second_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            second_thread_id,
            Some("attached-1".to_string()),
            Some("Thread B".to_string()),
        );

        let err = app
            .attach_oracle_thread_binding("attached-1".to_string(), None, None)
            .await
            .expect_err("duplicate local bindings should fail closed");

        assert!(err.to_string().contains("multiple local threads"));
        assert!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .attach_thread_calls
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn oracle_thread_history_requires_reattach_accepts_repaired_followup_errors() {
        assert!(App::oracle_thread_history_requires_reattach(
            "Browser follow-up parent session abc could not be found."
        ));
        assert!(App::oracle_thread_history_requires_reattach(
            "Session abc is missing browser metadata."
        ));
    }

    #[test]
    fn oracle_attach_history_message_mentions_bounded_remote_window() {
        let thread_id = ThreadId::new();
        let message = App::oracle_attach_history_message(
            &OracleAttachThreadBinding::AttachedRemote {
                thread_id,
                title: "Attached Thread".to_string(),
                conversation_id: "attached-1".to_string(),
                history: Vec::new(),
            },
            OracleHistoryImportOutcome::Imported { message_count: 2 },
            Some(&OracleBrokerThreadHistoryWindow {
                limit: 100,
                returned_count: 100,
                total_count: 135,
                truncated: true,
            }),
        );

        assert!(message.contains("Imported 2 prior message(s)."));
        assert!(message.contains("Remote history was limited to the newest 100 text message(s) out of at least 135 available."));
    }

    #[tokio::test]
    async fn import_oracle_thread_history_rejects_mismatched_bound_conversation() {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("remote-7".to_string()),
            Some("Remote Thread".to_string()),
        );

        let err = app
            .import_oracle_thread_history(
                None,
                thread_id,
                "wrong-9",
                vec![OracleBrokerThreadHistoryEntry {
                    role: "assistant".to_string(),
                    text: "Wrong thread answer".to_string(),
                }],
            )
            .await
            .expect_err("history import should fail closed on conversation mismatch");

        assert!(
            err.to_string()
                .contains("importing history for wrong-9 would cross streams")
        );
    }

    #[test]
    fn oracle_history_import_snapshot_refresh_required_for_buffered_or_merged_transcript() {
        assert!(App::oracle_history_import_requires_snapshot_refresh(
            true, false
        ));
        assert!(App::oracle_history_import_requires_snapshot_refresh(
            false, true
        ));
        assert!(!App::oracle_history_import_requires_snapshot_refresh(
            false, false
        ));
    }

    #[tokio::test]
    async fn attach_oracle_thread_binding_prefers_current_local_binding_session_for_remote_attach()
    -> Result<()> {
        let mut app = make_test_app().await;
        let first_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session(first_thread_id, Some("runtime-a".to_string()), true);
        let current_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session(
            current_thread_id,
            Some("runtime-b".to_string()),
            true,
        );
        app.activate_oracle_binding(current_thread_id);
        queue_test_oracle_attach_thread_result(
            &app,
            Ok(test_oracle_thread_open_response(
                "runtime-c",
                "Remote Thread",
                "remote-7",
            )),
        );

        let _ = app
            .attach_oracle_thread_binding("remote-7".to_string(), None, None)
            .await?;

        assert_eq!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .attach_thread_followup_sessions,
            vec![Some("runtime-b".to_string())]
        );
        Ok(())
    }

    #[tokio::test]
    async fn attach_oracle_thread_binding_times_out_remote_attach() {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session(thread_id, Some("runtime-root".to_string()), true);
        app.activate_oracle_binding(thread_id);
        queue_test_oracle_attach_thread_delay(&app, Duration::from_millis(75));

        let err = app
            .attach_oracle_thread_binding("remote-7".to_string(), None, None)
            .await
            .expect_err("slow remote attach should time out");

        assert_eq!(
            err.to_string(),
            "Timed out after 50 ms while attaching a remote Oracle thread."
        );
        assert!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .attach_thread_calls
                .is_empty()
        );
    }

    #[tokio::test]
    #[serial(oracle_timeout_env)]
    async fn oracle_remote_timeouts_honor_env_overrides() {
        let _list_timeout = EnvVarGuard::set("CODEX_ORACLE_PICKER_REMOTE_LIST_TIMEOUT_MS", "90000");
        let _mutation_timeout =
            EnvVarGuard::set("CODEX_ORACLE_REMOTE_THREAD_MUTATION_TIMEOUT_MS", "120000");
        let app = make_test_app().await;

        assert_eq!(
            app.oracle_picker_remote_list_timeout(),
            Duration::from_secs(90)
        );
        assert_eq!(
            app.oracle_remote_thread_mutation_timeout(),
            Duration::from_secs(120)
        );
    }

    #[tokio::test]
    async fn ensure_oracle_followup_session_for_run_reattaches_lost_remote_thread() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        queue_test_oracle_attach_thread_result(
            &app,
            Ok(test_oracle_thread_open_response(
                "runtime-reattached",
                "Attached Thread",
                "attached-1",
            )),
        );

        let followup_session = app
            .ensure_oracle_followup_session_for_run(thread_id)
            .await?;

        assert_eq!(followup_session.as_deref(), Some("runtime-reattached"));
        let hooks = app
            .oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks");
        assert_eq!(hooks.attach_thread_calls, vec!["attached-1".to_string()]);
        assert_eq!(hooks.attach_thread_followup_sessions, vec![None]);
        drop(hooks);
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&thread_id)
                .and_then(|binding| binding.current_session_id.as_deref()),
            Some("runtime-reattached")
        );
        Ok(())
    }

    #[tokio::test]
    async fn ensure_oracle_followup_session_for_run_rebinds_broker_owned_thread_session()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session_id(thread_id, Some("runtime-stale".to_string()));
        app.set_oracle_remote_metadata(
            thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        queue_test_oracle_attach_thread_result(
            &app,
            Ok(test_oracle_thread_open_response(
                "runtime-verified",
                "Attached Thread",
                "attached-1",
            )),
        );

        let followup_session = app
            .ensure_oracle_followup_session_for_run(thread_id)
            .await?;

        assert_eq!(followup_session.as_deref(), Some("runtime-verified"));
        let hooks = app
            .oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks");
        assert_eq!(hooks.attach_thread_calls, vec!["attached-1".to_string()]);
        assert_eq!(
            hooks.attach_thread_followup_sessions,
            vec![Some("runtime-stale".to_string())]
        );
        Ok(())
    }

    #[tokio::test]
    async fn ensure_oracle_followup_session_for_run_reuses_verified_broker_thread_session()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session(thread_id, Some("runtime-verified".to_string()), true);
        app.set_oracle_remote_metadata(
            thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );

        let followup_session = app
            .ensure_oracle_followup_session_for_run(thread_id)
            .await?;

        assert_eq!(followup_session.as_deref(), Some("runtime-verified"));
        let hooks = app
            .oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks");
        assert!(hooks.attach_thread_calls.is_empty());
        assert!(hooks.attach_thread_followup_sessions.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn ensure_oracle_followup_session_for_run_rejects_duplicate_conversation_binding()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("attached-1".to_string()),
            Some("Thread A".to_string()),
        );
        let conflicting_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            conflicting_thread_id,
            Some("attached-1".to_string()),
            Some("Thread B".to_string()),
        );
        app.set_oracle_thread_broker_session_id(
            conflicting_thread_id,
            Some("runtime-conflict".to_string()),
        );

        let err = app
            .ensure_oracle_followup_session_for_run(thread_id)
            .await
            .expect_err("duplicate local binding should fail closed");

        assert!(err.to_string().contains(&format!(
            "already bound to local thread {conflicting_thread_id}"
        )));
        assert!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .attach_thread_calls
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn ensure_oracle_session_continuity_for_run_restores_slug_after_interrupt_loss()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        queue_test_oracle_attach_thread_result(
            &app,
            Ok(OracleBrokerThreadOpenResponse {
                session_id: Some("runtime-reattached".to_string()),
                title: "Attached Thread".to_string(),
                conversation_id: Some("attached-1".to_string()),
                url: Some("https://chatgpt.com/c/attached-1".to_string()),
                history: Vec::new(),
            }),
        );

        let (session_slug, followup_session) = app
            .ensure_oracle_session_continuity_for_run(thread_id)
            .await?;

        assert_eq!(followup_session.as_deref(), Some("runtime-reattached"));
        assert!(!session_slug.is_empty());
        assert_eq!(
            app.oracle_state.session_root_slug.as_deref(),
            Some(session_slug.as_str())
        );
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&thread_id)
                .and_then(|binding| binding.session_root_slug.as_deref()),
            Some(session_slug.as_str())
        );
        Ok(())
    }

    #[tokio::test]
    async fn open_oracle_picker_keeps_local_controls_available_when_remote_threads_fail()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        queue_test_oracle_list_threads_result(&app, Err("broker offline".to_string()));

        app.open_oracle_picker().await?;
        let initial_popup = render_bottom_popup(&app, /*width*/ 100);
        assert!(initial_popup.contains("Fetching remote threads"));
        assert!(initial_popup.contains("new"));
        handle_next_oracle_picker_remote_list_result(&mut app, &mut app_event_rx).await?;

        let popup = render_bottom_popup(&app, /*width*/ 100);
        assert!(popup.contains("Remote threads unavailable"));
        assert!(popup.contains("broker offline"));
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::OraclePickerToggleInfo)
        );
        assert!(popup.contains("Hotkeys:"));
        assert!(popup.contains("new"));
        assert!(!popup.contains("search"));
        Ok(())
    }

    #[tokio::test]
    async fn open_oracle_picker_renders_immediately_before_remote_threads_finish() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        queue_test_oracle_list_threads_delay(&app, Duration::from_millis(25));
        queue_test_oracle_list_threads_result(
            &app,
            Ok(vec![OracleBrokerThreadEntry {
                title: "Slow Thread".to_string(),
                conversation_id: "slow-1".to_string(),
                url: Some("https://chatgpt.com/c/slow-1".to_string()),
                is_current: false,
            }]),
        );

        app.open_oracle_picker().await?;

        let popup = render_bottom_popup(&app, /*width*/ 100);
        assert!(popup.contains("Fetching remote threads"));
        assert!(popup.contains("Hotkeys:"));
        assert!(popup.contains("new"));
        assert!(!popup.contains("Slow Thread"));
        assert_eq!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .list_thread_calls,
            0,
            "remote list should not complete before the first picker render"
        );

        handle_next_oracle_picker_remote_list_result(&mut app, &mut app_event_rx).await?;
        let popup = render_bottom_popup(&app, /*width*/ 100);
        assert!(popup.contains("Remote: Slow Thread"));
        Ok(())
    }

    #[tokio::test]
    async fn open_oracle_picker_timeout_keeps_new_thread_hotkey_available() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        queue_test_oracle_list_threads_delay(&app, Duration::from_millis(75));
        queue_test_oracle_list_threads_result(
            &app,
            Ok(vec![OracleBrokerThreadEntry {
                title: "Slow Thread".to_string(),
                conversation_id: "slow-1".to_string(),
                url: Some("https://chatgpt.com/c/slow-1".to_string()),
                is_current: false,
            }]),
        );

        app.open_oracle_picker().await?;
        handle_next_oracle_picker_remote_list_result(&mut app, &mut app_event_rx).await?;

        let popup = render_bottom_popup(&app, /*width*/ 100);
        assert!(popup.contains("Remote threads unavailable"));
        assert!(popup.contains("Timed out after 50 ms"));
        assert!(popup.contains("Hotkeys:"));
        assert!(popup.contains("new"));
        assert!(!popup.contains("Slow Thread"));
        Ok(())
    }

    #[tokio::test]
    async fn open_oracle_picker_prefers_current_local_binding_for_remote_browse() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let first_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session(first_thread_id, Some("runtime-a".to_string()), true);
        let current_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session(
            current_thread_id,
            Some("runtime-b".to_string()),
            true,
        );
        app.activate_oracle_binding(current_thread_id);
        queue_test_oracle_list_threads_result(&app, Ok(Vec::new()));

        app.open_oracle_picker().await?;
        handle_next_oracle_picker_remote_list_result(&mut app, &mut app_event_rx).await?;

        assert_eq!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .list_thread_followup_sessions,
            vec![Some("runtime-b".to_string())]
        );
        Ok(())
    }

    #[tokio::test]
    async fn open_oracle_picker_does_not_borrow_another_binding_session_for_remote_browse()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let attached_thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_thread_broker_session_id(attached_thread_id, Some("runtime-a".to_string()));
        let unattached_thread_id = app.create_visible_oracle_thread().await;
        app.activate_oracle_binding(unattached_thread_id);
        queue_test_oracle_list_threads_result(&app, Ok(Vec::new()));

        app.open_oracle_picker().await?;
        handle_next_oracle_picker_remote_list_result(&mut app, &mut app_event_rx).await?;

        assert_eq!(
            app.oracle_test_broker_hooks
                .lock()
                .expect("oracle test broker hooks")
                .list_thread_followup_sessions,
            vec![None]
        );
        Ok(())
    }

    #[tokio::test]
    async fn open_oracle_picker_without_attached_session_keeps_new_thread_hotkey_available()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        queue_test_oracle_list_threads_result(&app, Ok(Vec::new()));

        app.open_oracle_picker().await?;
        handle_next_oracle_picker_remote_list_result(&mut app, &mut app_event_rx).await?;

        let popup = render_bottom_popup(&app, /*width*/ 100);
        assert!(popup.contains("Hotkeys:"));
        assert!(popup.contains("new"));
        assert!(!popup.contains("search"));
        Ok(())
    }

    #[tokio::test]
    async fn oracle_picker_search_shortcut_filters_threads_by_topic() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;

        app.show_oracle_picker(
            vec![
                OracleBrokerThreadEntry {
                    title: "Bug triage".to_string(),
                    conversation_id: "bug-1".to_string(),
                    url: Some("https://chatgpt.com/c/bug-1".to_string()),
                    is_current: false,
                },
                OracleBrokerThreadEntry {
                    title: "Fresh Thread".to_string(),
                    conversation_id: "fresh-2".to_string(),
                    url: Some("https://chatgpt.com/c/fresh-2".to_string()),
                    is_current: false,
                },
            ],
            /*include_new_thread*/ true,
        );

        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        for ch in ['f', 'r', 'e', 's', 'h'] {
            app.chat_widget
                .handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::OracleAttachThread {
                conversation_id,
                title,
                url,
            }) if conversation_id == "fresh-2"
                && title == "Fresh Thread"
                && url.as_deref() == Some("https://chatgpt.com/c/fresh-2")
        );
    }

    #[tokio::test]
    async fn oracle_picker_tab_shortcut_toggles_model_preference() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        app.oracle_state.model = OracleModelPreset::Pro;

        app.show_oracle_picker(Vec::new(), /*include_new_thread*/ true);
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::OraclePickerToggleModel)
        );
    }

    #[tokio::test]
    async fn oracle_picker_info_toggle_refreshes_popup_in_place() {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.show_oracle_picker(Vec::new(), /*include_new_thread*/ true);

        assert!(!render_bottom_popup(&app, /*width*/ 100).contains("Model:"));

        assert!(app.toggle_oracle_picker_info());
        let popup = render_bottom_popup(&app, /*width*/ 100);
        assert!(popup.contains("Model: GPT-5.5 Pro"));
        assert!(popup.contains("Phase: waiting for Oracle user-turn reply"));
        assert!(popup.contains("Attached: 1"));
        assert!(popup.contains("Remote: 0"));

        assert!(app.toggle_oracle_picker_info());
        assert!(!render_bottom_popup(&app, /*width*/ 100).contains("Model:"));
    }

    #[tokio::test]
    async fn oracle_picker_toggle_model_refreshes_popup_in_place() {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.activate_oracle_binding(thread_id);
        app.show_oracle_picker(Vec::new(), /*include_new_thread*/ true);

        assert_eq!(app.oracle_state.model, OracleModelPreset::Pro);
        assert!(render_bottom_popup(&app, /*width*/ 100).contains("thinking 5.5"));

        assert!(app.toggle_oracle_picker_model().await);
        assert_eq!(app.oracle_state.model, OracleModelPreset::Thinking);
        let popup = render_bottom_popup(&app, /*width*/ 100);
        assert!(popup.contains("gpt-5.5 pro"));
        assert!(!popup.contains("thinking 5.5"));
    }

    #[tokio::test]
    async fn contextual_model_popup_shows_oracle_models_for_visible_oracle_thread() {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let session = app.oracle_thread_session(thread_id);
        app.activate_thread_channel(thread_id).await;
        app.chat_widget.handle_thread_session(session);

        app.open_contextual_model_popup();

        let popup = render_bottom_popup(&app, /*width*/ 100);
        assert!(popup.contains("Oracle Model"));
        assert!(popup.contains("GPT-5.5 Pro"));
        assert!(popup.contains("Thinking 5.5"));
    }

    #[tokio::test]
    async fn contextual_model_popup_selecting_pro_opens_oracle_pro_reasoning_popup() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let session = app.oracle_thread_session(thread_id);
        app.activate_thread_channel(thread_id).await;
        app.chat_widget.handle_thread_session(session);
        while app_event_rx.try_recv().is_ok() {}

        app.open_contextual_model_popup();
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::OpenOracleProReasoningPopup)
        );
    }

    #[tokio::test]
    async fn oracle_pro_reasoning_popup_shows_standard_and_extended_options() {
        let mut app = make_test_app().await;
        app.oracle_state.model = OracleModelPreset::ProExtended;

        app.open_oracle_pro_reasoning_popup();

        let popup = render_bottom_popup(&app, /*width*/ 100);
        assert!(popup.contains("Oracle Pro Thinking"));
        assert!(popup.contains("Standard"));
        assert!(popup.contains("Extended"));
        assert!(popup.contains("GPT-5.5 Pro (Extended)"));
    }

    #[tokio::test]
    async fn oracle_picker_new_shortcut_closes_modal_before_creating_thread() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        app.show_oracle_picker(Vec::new(), /*include_new_thread*/ true);

        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

        assert_matches!(app_event_rx.try_recv(), Ok(AppEvent::OracleCreateThread));
        assert!(
            !app.chat_widget
                .selection_view_is_active(ORACLE_SELECTION_VIEW_ID)
        );
    }

    #[tokio::test]
    async fn set_oracle_model_preference_updates_all_visible_oracle_thread_sessions() -> Result<()>
    {
        let mut app = make_test_app().await;
        let first_thread_id = app.create_visible_oracle_thread().await;
        let second_thread_id = app.create_visible_oracle_thread().await;
        app.activate_oracle_binding(first_thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint);
        app.persist_active_oracle_binding();

        let status = app
            .set_oracle_model_preference(OracleModelPreset::Thinking)
            .await;

        assert_eq!(app.oracle_state.model, OracleModelPreset::Thinking);
        assert!(status.contains("current in-flight step will finish"));
        for thread_id in [first_thread_id, second_thread_id] {
            let channel = app
                .thread_event_channels
                .get(&thread_id)
                .expect("oracle thread channel");
            let store = channel.store.lock().await;
            assert_eq!(
                store.session.as_ref().map(|session| session.model.as_str()),
                Some("requested gpt-5.5 (Thinking 5.5)")
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn set_oracle_model_preference_pro_extended_updates_visible_oracle_thread_sessions()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        app.activate_oracle_binding(thread_id);

        let status = app
            .set_oracle_model_preference(OracleModelPreset::ProExtended)
            .await;

        assert_eq!(app.oracle_state.model, OracleModelPreset::ProExtended);
        assert!(status.contains("gpt-5.5-pro (Extended)"));
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert_eq!(
            store.session.as_ref().map(|session| session.model.as_str()),
            Some("requested gpt-5.5-pro (Extended)")
        );
        Ok(())
    }

    #[tokio::test]
    async fn sync_oracle_thread_session_updates_visible_widget_when_active_thread_lags() {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let initial_session = app.oracle_thread_session(oracle_thread_id);
        app.chat_widget.handle_thread_session(initial_session);

        app.active_thread_id = Some(ThreadId::new());
        app.oracle_state.model = OracleModelPreset::Thinking;

        app.sync_oracle_thread_session(oracle_thread_id).await;

        assert_eq!(
            app.chat_widget.thread_id(),
            Some(oracle_thread_id),
            "the widget should still be displaying the oracle thread"
        );
        assert_eq!(
            app.chat_widget.current_model(),
            "requested gpt-5.5 (Thinking 5.5)"
        );
    }

    #[tokio::test]
    async fn submit_active_thread_op_prefers_displayed_oracle_thread_when_active_thread_lags()
    -> Result<()> {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        let main_thread_id = ThreadId::new();
        let main_session = test_thread_session(main_thread_id, test_path_buf("/tmp/project"));
        app.chat_widget.handle_thread_session(main_session.clone());
        app.thread_event_channels.insert(
            main_thread_id,
            ThreadEventChannel::new_with_session(
                THREAD_EVENT_CHANNEL_CAPACITY,
                main_session,
                Vec::new(),
            ),
        );
        app.activate_thread_channel(main_thread_id).await;
        app.primary_thread_id = Some(main_thread_id);

        std::fs::create_dir_all(app.chat_widget.config_ref().cwd.as_path())?;
        let oracle_repo =
            find_oracle_repo(app.chat_widget.config_ref().cwd.as_path()).expect("oracle repo");
        app.oracle_broker = Some((oracle_repo, OracleBrokerClient::new_test_client()));
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        app.chat_widget
            .handle_thread_session(app.oracle_thread_session(oracle_thread_id));

        while op_rx.try_recv().is_ok() {}

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.submit_active_thread_op(
            &mut app_server,
            AppCommand::user_turn(
                vec![UserInput::Text {
                    text: "Reply with exactly routed-through-oracle and nothing else.".to_string(),
                    text_elements: Vec::new(),
                }],
                app.chat_widget.config_ref().cwd.to_path_buf(),
                app.chat_widget
                    .config_ref()
                    .permissions
                    .approval_policy
                    .value(),
                app.chat_widget.config_ref().legacy_sandbox_policy(),
                Some(
                    app.chat_widget
                        .config_ref()
                        .permissions
                        .permission_profile(),
                ),
                app.oracle_thread_session(oracle_thread_id).model,
                None,
                None,
                app.chat_widget.config_ref().service_tier.map(Some),
                None,
                None,
                app.chat_widget.config_ref().personality,
            ),
        )
        .await?;

        assert_eq!(app.active_thread_id, Some(main_thread_id));
        assert_eq!(app.chat_widget.thread_id(), Some(oracle_thread_id));
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn)
        );
        assert!(app.oracle_state.active_run_id.is_some());
        assert!(app.oracle_state.pending_turn_id.is_some());
        let request = app
            .oracle_state
            .inflight_run_requests
            .values()
            .next()
            .expect("oracle request");
        assert_eq!(request.oracle_thread_id, oracle_thread_id);
        assert_eq!(request.kind, OracleRequestKind::UserTurn);
        assert_eq!(
            request.source_user_text.as_deref(),
            Some("Reply with exactly routed-through-oracle and nothing else.")
        );
        while let Ok(op) = op_rx.try_recv() {
            assert!(
                !matches!(op, Op::UserTurn { .. }),
                "displayed oracle thread should not emit app-server UserTurn ops",
            );
        }

        app.shutdown_oracle_broker(true);
        Ok(())
    }

    #[tokio::test]
    async fn binding_oracle_to_visible_thread_reuses_existing_thread_and_preserves_state() {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();

        app.oracle_state.session_root_slug = Some("oracle-session-a".to_string());
        app.oracle_state.current_session_id = Some("oracle-session-a-2".to_string());
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.oracle_state.last_orchestrator_task = Some("ship milestone a".to_string());
        app.oracle_state.phase = OracleSupervisorPhase::WaitingForOrchestrator;
        let reused_thread_id = app.ensure_visible_oracle_thread().await;

        assert_eq!(reused_thread_id, thread_id);
        assert_eq!(app.oracle_state.oracle_thread_id, Some(thread_id));
        assert_eq!(
            app.oracle_state.session_root_slug.as_deref(),
            Some("oracle-session-a")
        );
        assert_eq!(
            app.oracle_state.current_session_id.as_deref(),
            Some("oracle-session-a-2")
        );
        assert_eq!(
            app.oracle_state.orchestrator_thread_id,
            Some(orchestrator_thread_id)
        );
        assert_eq!(
            app.oracle_state.last_orchestrator_task.as_deref(),
            Some("ship milestone a")
        );
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOrchestrator
        );
    }

    #[tokio::test]
    async fn oracle_followup_session_requires_thread_owned_session_state() {
        let mut app = make_test_app().await;
        let thread_id = app.create_visible_oracle_thread().await;
        {
            let binding = app
                .oracle_state
                .bindings
                .get_mut(&thread_id)
                .expect("oracle binding");
            binding.current_session_id = Some("stale-hidden-session".to_string());
        }

        assert_eq!(app.oracle_followup_session_for_thread(thread_id), None);

        {
            let binding = app
                .oracle_state
                .bindings
                .get_mut(&thread_id)
                .expect("oracle binding");
            binding.current_session_id = Some("runtime-blank".to_string());
            binding.current_session_ownership = Some(OracleSessionOwnership::BrokerThread);
        }
        assert_eq!(
            app.oracle_followup_session_for_thread(thread_id).as_deref(),
            Some("runtime-blank")
        );

        {
            let binding = app
                .oracle_state
                .bindings
                .get_mut(&thread_id)
                .expect("oracle binding");
            binding.current_session_id = Some("stale-hidden-session".to_string());
            binding.conversation_id = Some("remote-1".to_string());
            binding.current_session_ownership = Some(OracleSessionOwnership::BrokerThread);
        }
        assert_eq!(
            app.oracle_followup_session_for_thread(thread_id).as_deref(),
            Some("stale-hidden-session")
        );

        {
            let binding = app
                .oracle_state
                .bindings
                .get_mut(&thread_id)
                .expect("oracle binding");
            binding.current_session_ownership = None;
            binding.session_root_slug = Some("oracle-thread-1".to_string());
            binding.current_session_id = Some("manual-check-codex-oracle-handoff".to_string());
        }
        assert_eq!(app.oracle_followup_session_for_thread(thread_id), None);

        {
            let binding = app
                .oracle_state
                .bindings
                .get_mut(&thread_id)
                .expect("oracle binding");
            binding.current_session_id = Some("oracle-thread-1-2".to_string());
            binding.current_session_ownership = Some(OracleSessionOwnership::SessionRootSlug(
                "oracle-thread-1".to_string(),
            ));
        }
        assert_eq!(
            app.oracle_followup_session_for_thread(thread_id).as_deref(),
            Some("oracle-thread-1-2")
        );
    }

    #[tokio::test]
    async fn hidden_oracle_orchestrator_thread_is_omitted_from_agent_picker() {
        let mut app = make_test_app().await;
        app.ensure_visible_oracle_thread().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let hidden_thread_id = app
            .ensure_orchestrator_thread(&mut app_server)
            .await
            .expect("orchestrator thread");

        assert!(app.is_hidden_oracle_thread(hidden_thread_id));
        assert!(app.agent_navigation.get(&hidden_thread_id).is_none());
    }

    #[tokio::test]
    async fn hidden_oracle_participant_thread_stays_omitted_after_owner_fields_clear() {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let hidden_thread_id = ThreadId::new();

        if let Some(binding) = app.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.participants.insert(
                ORACLE_ORCHESTRATOR_DESTINATION.to_string(),
                OracleRoutingParticipant {
                    address: ORACLE_ORCHESTRATOR_DESTINATION.to_string(),
                    thread_id: Some(hidden_thread_id),
                    title: Some("Orchestrator".to_string()),
                    kind: Some("orchestrator".to_string()),
                    role: Some("orchestrator".to_string()),
                    visibility: OracleParticipantVisibility::Hidden,
                    owned_by_oracle: true,
                    route_completions: false,
                    route_closures: false,
                    last_task: Some("still private".to_string()),
                },
            );
        }

        app.upsert_agent_picker_thread(
            hidden_thread_id,
            Some("Orchestrator".to_string()),
            Some("orchestrator".to_string()),
            /*is_closed*/ false,
        );

        assert!(app.agent_navigation.get(&hidden_thread_id).is_none());
    }

    #[tokio::test]
    async fn hidden_oracle_orchestrator_thread_closed_while_waiting_emits_checkpoint_event()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let hidden_thread_id = ThreadId::new();

        app.oracle_state.orchestrator_thread_id = Some(hidden_thread_id);
        app.oracle_state.phase = OracleSupervisorPhase::WaitingForOrchestrator;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 7,
            orchestrator_thread_id: Some(hidden_thread_id),
            ..Default::default()
        });
        app.persist_active_oracle_binding();
        app.thread_event_channels
            .insert(hidden_thread_id, ThreadEventChannel::new(/*capacity*/ 1));

        app.enqueue_thread_notification(
            hidden_thread_id,
            thread_closed_notification(hidden_thread_id),
        )
        .await?;

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::OracleCheckpoint {
                thread_id,
                workflow_version,
            }) if thread_id == hidden_thread_id && workflow_version == Some(7)
        );
        app.activate_oracle_binding(oracle_thread_id);
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOrchestrator
        );
        assert_eq!(
            app.oracle_state.orchestrator_thread_id,
            Some(hidden_thread_id)
        );
        assert!(
            app.oracle_state
                .last_status
                .as_deref()
                .is_none_or(|status| !status.contains("closed before reporting back"))
        );
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&oracle_thread_id)
                .expect("oracle binding")
                .pending_checkpoint_threads,
            vec![hidden_thread_id]
        );
        Ok(())
    }

    #[tokio::test]
    async fn hidden_oracle_orchestrator_thread_closed_during_active_checkpoint_review_stays_preserved()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let hidden_thread_id = ThreadId::new();

        app.oracle_state.orchestrator_thread_id = Some(hidden_thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint);
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 7,
            orchestrator_thread_id: Some(hidden_thread_id),
            ..Default::default()
        });
        app.oracle_state.active_checkpoint_thread_id = Some(hidden_thread_id);
        app.oracle_state.active_checkpoint_turn_id = Some("orchestrator-turn-1".to_string());
        app.persist_active_oracle_binding();
        if let Some(binding) = app.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.active_checkpoint_thread_id = Some(hidden_thread_id);
            binding.active_checkpoint_turn_id = Some("orchestrator-turn-1".to_string());
        }
        app.persist_active_oracle_binding();
        app.thread_event_channels
            .insert(hidden_thread_id, ThreadEventChannel::new(/*capacity*/ 1));

        app.enqueue_thread_notification(
            hidden_thread_id,
            thread_closed_notification(hidden_thread_id),
        )
        .await?;

        assert!(
            tokio::time::timeout(Duration::from_millis(50), app_event_rx.recv())
                .await
                .is_err(),
            "active checkpoint review should stay queued locally instead of emitting a duplicate checkpoint event"
        );
        app.activate_oracle_binding(oracle_thread_id);
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint)
        );
        assert_eq!(
            app.oracle_state.orchestrator_thread_id,
            Some(hidden_thread_id)
        );
        assert_eq!(
            app.oracle_state.active_checkpoint_thread_id,
            Some(hidden_thread_id)
        );
        assert!(
            app.oracle_state
                .last_status
                .as_deref()
                .is_none_or(|status| !status.contains("closed before reporting back"))
        );
        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert_eq!(binding.pending_checkpoint_threads, vec![hidden_thread_id]);
        assert_eq!(binding.active_checkpoint_thread_id, Some(hidden_thread_id));
        Ok(())
    }

    #[tokio::test]
    async fn hidden_oracle_orchestrator_thread_closed_clears_idle_binding_owner() -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let hidden_thread_id = ThreadId::new();

        app.oracle_state.orchestrator_thread_id = Some(hidden_thread_id);
        app.oracle_state.phase = OracleSupervisorPhase::Idle;
        app.persist_active_oracle_binding();
        app.thread_event_channels
            .insert(hidden_thread_id, ThreadEventChannel::new(/*capacity*/ 1));

        app.enqueue_thread_notification(
            hidden_thread_id,
            thread_closed_notification(hidden_thread_id),
        )
        .await?;

        app.activate_oracle_binding(oracle_thread_id);
        assert_eq!(app.oracle_state.orchestrator_thread_id, None);
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&oracle_thread_id)
                .and_then(|binding| binding.orchestrator_thread_id),
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn hidden_oracle_thread_closure_preserves_failed_workflow_outside_waiting_phase()
    -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let hidden_thread_id = ThreadId::new();

        app.oracle_state.orchestrator_thread_id = Some(hidden_thread_id);
        app.oracle_state.phase = OracleSupervisorPhase::Idle;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 3,
            orchestrator_thread_id: Some(hidden_thread_id),
            ..Default::default()
        });
        app.persist_active_oracle_binding();
        app.thread_event_channels
            .insert(hidden_thread_id, ThreadEventChannel::new(/*capacity*/ 1));

        app.enqueue_thread_notification(
            hidden_thread_id,
            thread_closed_notification(hidden_thread_id),
        )
        .await?;

        app.activate_oracle_binding(oracle_thread_id);
        let workflow = app
            .oracle_state
            .workflow
            .as_ref()
            .expect("workflow preserved");
        assert_eq!(workflow.status, OracleWorkflowStatus::Failed);
        assert_eq!(workflow.orchestrator_thread_id, None);
        assert_eq!(
            workflow.last_blocker.as_deref(),
            Some("The orchestrator thread closed before reporting back.")
        );
        Ok(())
    }

    #[tokio::test]
    async fn hidden_oracle_orchestrator_idle_status_does_not_emit_checkpoint_event() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let hidden_thread_id = ThreadId::new();

        app.oracle_state.orchestrator_thread_id = Some(hidden_thread_id);
        app.oracle_state.phase = OracleSupervisorPhase::WaitingForOrchestrator;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 7,
            orchestrator_thread_id: Some(hidden_thread_id),
            ..Default::default()
        });
        app.persist_active_oracle_binding();
        app.thread_event_channels
            .insert(hidden_thread_id, ThreadEventChannel::new(/*capacity*/ 1));
        while app_event_rx.try_recv().is_ok() {}

        app.enqueue_thread_notification(
            hidden_thread_id,
            thread_status_changed_notification(
                hidden_thread_id,
                codex_app_server_protocol::ThreadStatus::Idle,
            ),
        )
        .await?;

        assert!(
            tokio::time::timeout(Duration::from_millis(50), app_event_rx.recv())
                .await
                .is_err(),
            "idle status should not trigger a checkpoint event by itself"
        );
        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert!(binding.pending_checkpoint_threads.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn hidden_oracle_orchestrator_new_turn_emits_checkpoint_when_stale_pending_exists()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let hidden_thread_id = ThreadId::new();

        app.oracle_state.orchestrator_thread_id = Some(hidden_thread_id);
        app.oracle_state.phase = OracleSupervisorPhase::WaitingForOrchestrator;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 8,
            orchestrator_thread_id: Some(hidden_thread_id),
            ..Default::default()
        });
        app.persist_active_oracle_binding();
        app.thread_event_channels
            .insert(hidden_thread_id, ThreadEventChannel::new(/*capacity*/ 1));
        if let Some(binding) = app.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.pending_checkpoint_threads = vec![hidden_thread_id];
            binding
                .pending_checkpoint_versions
                .insert(hidden_thread_id, 7);
            binding
                .pending_checkpoint_turn_ids
                .insert(hidden_thread_id, "turn-1".to_string());
            binding
                .last_checkpoint_turn_ids
                .insert(hidden_thread_id, "turn-1".to_string());
        }
        app.persist_active_oracle_binding();

        app.enqueue_thread_notification(
            hidden_thread_id,
            turn_completed_notification(hidden_thread_id, "turn-2", TurnStatus::Completed),
        )
        .await?;

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::OracleCheckpoint {
                thread_id,
                workflow_version,
            }) if thread_id == hidden_thread_id && workflow_version == Some(8)
        );
        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert_eq!(
            binding
                .pending_checkpoint_turn_ids
                .get(&hidden_thread_id)
                .map(String::as_str),
            Some("turn-2")
        );
        Ok(())
    }

    #[tokio::test]
    async fn orchestrator_checkpoint_without_terminal_turn_preserves_pending_and_retries()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let hidden_thread_id = app.ensure_orchestrator_thread(&mut app_server).await?;

        app.oracle_state.session_root_slug = Some("oracle-session-a".to_string());
        app.oracle_state.orchestrator_thread_id = Some(hidden_thread_id);
        app.oracle_state.phase = OracleSupervisorPhase::WaitingForOrchestrator;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 7,
            orchestrator_thread_id: Some(hidden_thread_id),
            ..Default::default()
        });
        app.persist_active_oracle_binding();
        if let Some(binding) = app.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.pending_checkpoint_threads = vec![hidden_thread_id];
            binding
                .pending_checkpoint_versions
                .insert(hidden_thread_id, 7);
            binding
                .pending_checkpoint_turn_ids
                .insert(hidden_thread_id, "orchestrator-turn-1".to_string());
        }
        app.persist_active_oracle_binding();
        while app_event_rx.try_recv().is_ok() {}

        app.handle_orchestrator_checkpoint(&mut app_server, hidden_thread_id, Some(7))
            .await?;

        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert_eq!(binding.pending_checkpoint_threads, vec![hidden_thread_id]);
        assert_eq!(
            binding.pending_checkpoint_versions.get(&hidden_thread_id),
            Some(&7)
        );
        assert_eq!(
            binding
                .pending_checkpoint_turn_ids
                .get(&hidden_thread_id)
                .map(String::as_str),
            Some("orchestrator-turn-1")
        );
        assert_matches!(
            tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv()).await,
            Ok(Some(AppEvent::OracleCheckpoint {
                thread_id,
                workflow_version,
            })) if thread_id == hidden_thread_id && workflow_version == Some(7)
        );
        Ok(())
    }

    #[tokio::test]
    async fn orchestrator_checkpoint_without_terminal_turn_retries_even_before_turn_id_is_known()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let hidden_thread_id = app.ensure_orchestrator_thread(&mut app_server).await?;

        app.oracle_state.session_root_slug = Some("oracle-session-a".to_string());
        app.oracle_state.orchestrator_thread_id = Some(hidden_thread_id);
        app.oracle_state.phase = OracleSupervisorPhase::WaitingForOrchestrator;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 7,
            orchestrator_thread_id: Some(hidden_thread_id),
            ..Default::default()
        });
        app.persist_active_oracle_binding();
        if let Some(binding) = app.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.pending_checkpoint_threads = vec![hidden_thread_id];
            binding
                .pending_checkpoint_versions
                .insert(hidden_thread_id, 7);
        }
        app.persist_active_oracle_binding();
        while app_event_rx.try_recv().is_ok() {}

        app.handle_orchestrator_checkpoint(&mut app_server, hidden_thread_id, Some(7))
            .await?;

        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert_eq!(binding.pending_checkpoint_threads, vec![hidden_thread_id]);
        assert_eq!(
            binding.pending_checkpoint_versions.get(&hidden_thread_id),
            Some(&7)
        );
        assert_matches!(
            tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv()).await,
            Ok(Some(AppEvent::OracleCheckpoint {
                thread_id,
                workflow_version,
            })) if thread_id == hidden_thread_id && workflow_version == Some(7)
        );
        Ok(())
    }

    #[tokio::test]
    async fn hidden_oracle_orchestrator_close_preserves_pending_checkpoint() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let hidden_thread_id = ThreadId::new();

        app.oracle_state.orchestrator_thread_id = Some(hidden_thread_id);
        app.oracle_state.phase = OracleSupervisorPhase::WaitingForOrchestrator;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 7,
            orchestrator_thread_id: Some(hidden_thread_id),
            ..Default::default()
        });
        app.persist_active_oracle_binding();
        app.thread_event_channels
            .insert(hidden_thread_id, ThreadEventChannel::new(/*capacity*/ 1));

        app.enqueue_thread_notification(
            hidden_thread_id,
            turn_completed_notification(hidden_thread_id, "turn-1", TurnStatus::Completed),
        )
        .await?;

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::OracleCheckpoint {
                thread_id,
                workflow_version,
            }) if thread_id == hidden_thread_id && workflow_version == Some(7)
        );
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&oracle_thread_id)
                .expect("oracle binding")
                .pending_checkpoint_threads,
            vec![hidden_thread_id]
        );

        app.enqueue_thread_notification(
            hidden_thread_id,
            thread_closed_notification(hidden_thread_id),
        )
        .await?;

        app.activate_oracle_binding(oracle_thread_id);
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOrchestrator
        );
        assert_eq!(
            app.oracle_state.orchestrator_thread_id,
            Some(hidden_thread_id)
        );
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&oracle_thread_id)
                .expect("oracle binding")
                .pending_checkpoint_threads,
            vec![hidden_thread_id]
        );
        assert!(
            app.oracle_state
                .last_status
                .as_deref()
                .is_none_or(|status| { !status.contains("closed before reporting back") })
        );
        Ok(())
    }

    #[tokio::test]
    async fn hidden_oracle_orchestrator_close_before_first_turn_fails_instead_of_retrying_forever()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let hidden_thread_id = app.ensure_orchestrator_thread(&mut app_server).await?;

        app.oracle_state.session_root_slug = Some("oracle-session-a".to_string());
        app.oracle_state.orchestrator_thread_id = Some(hidden_thread_id);
        app.oracle_state.phase = OracleSupervisorPhase::WaitingForOrchestrator;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 7,
            orchestrator_thread_id: Some(hidden_thread_id),
            ..Default::default()
        });
        app.persist_active_oracle_binding();
        while app_event_rx.try_recv().is_ok() {}

        app.enqueue_thread_notification(
            hidden_thread_id,
            thread_closed_notification(hidden_thread_id),
        )
        .await?;

        assert_matches!(
            tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv()).await,
            Ok(Some(AppEvent::OracleCheckpoint {
                thread_id,
                workflow_version,
            })) if thread_id == hidden_thread_id && workflow_version == Some(7)
        );

        app.handle_orchestrator_checkpoint(&mut app_server, hidden_thread_id, Some(7))
            .await?;

        app.activate_oracle_binding(oracle_thread_id);
        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.orchestrator_thread_id, None);
        assert_eq!(
            app.oracle_state.last_status.as_deref(),
            Some(
                format!(
                    "The orchestrator thread {hidden_thread_id} closed before reporting back. Oracle supervision is idle again."
                )
                .as_str()
            )
        );
        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert!(binding.pending_checkpoint_threads.is_empty());
        assert!(
            !binding
                .pending_checkpoint_versions
                .contains_key(&hidden_thread_id)
        );
        assert!(
            !binding
                .pending_checkpoint_turn_ids
                .contains_key(&hidden_thread_id)
        );
        assert_eq!(binding.orchestrator_thread_id, None);
        let workflow = binding.workflow.as_ref().expect("workflow");
        assert_eq!(workflow.status, OracleWorkflowStatus::Failed);
        assert_eq!(workflow.orchestrator_thread_id, None);
        assert_eq!(
            workflow.last_blocker.as_deref(),
            Some("The orchestrator thread closed before reporting back.")
        );
        Ok(())
    }

    #[tokio::test]
    async fn terminal_orchestrator_checkpoint_read_failure_fails_closed_instead_of_bubbling()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let hidden_thread_id = ThreadId::new();

        app.oracle_state.session_root_slug = Some("oracle-session-a".to_string());
        app.oracle_state.orchestrator_thread_id = Some(hidden_thread_id);
        app.oracle_state.phase = OracleSupervisorPhase::WaitingForOrchestrator;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 7,
            orchestrator_thread_id: Some(hidden_thread_id),
            ..Default::default()
        });
        app.persist_active_oracle_binding();
        if let Some(binding) = app.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.pending_checkpoint_threads = vec![hidden_thread_id];
            binding
                .pending_checkpoint_versions
                .insert(hidden_thread_id, 7);
            binding
                .pending_checkpoint_turn_ids
                .insert(hidden_thread_id, "orchestrator-turn-1".to_string());
        }
        app.persist_active_oracle_binding();
        while app_event_rx.try_recv().is_ok() {}

        app.handle_orchestrator_checkpoint(&mut app_server, hidden_thread_id, Some(7))
            .await?;

        app.activate_oracle_binding(oracle_thread_id);
        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.orchestrator_thread_id, None);
        assert_eq!(
            app.oracle_state.last_status.as_deref(),
            Some(
                format!(
                    "The orchestrator thread {hidden_thread_id} closed before reporting back. Oracle supervision is idle again."
                )
                .as_str()
            )
        );
        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert!(binding.pending_checkpoint_threads.is_empty());
        assert!(
            !binding
                .pending_checkpoint_versions
                .contains_key(&hidden_thread_id)
        );
        assert!(
            !binding
                .pending_checkpoint_turn_ids
                .contains_key(&hidden_thread_id)
        );
        assert_eq!(binding.orchestrator_thread_id, None);
        let workflow = binding.workflow.as_ref().expect("workflow");
        assert_eq!(workflow.status, OracleWorkflowStatus::Failed);
        assert_eq!(workflow.orchestrator_thread_id, None);
        assert_eq!(
            workflow.last_blocker.as_deref(),
            Some("The orchestrator thread closed before reporting back.")
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), app_event_rx.recv())
                .await
                .is_err(),
            "terminal checkpoint read should fail closed locally without queueing another retry"
        );
        Ok(())
    }

    #[test]
    fn next_orchestrator_checkpoint_turn_id_skips_non_terminal_and_duplicate_turns() {
        let thread_id = ThreadId::new();
        let base_thread = Thread {
            id: thread_id.to_string(),
            forked_from_id: None,
            preview: "orchestrator thread".to_string(),
            ephemeral: false,
            model_provider: "openai".to_string(),
            created_at: 1,
            updated_at: 1,
            status: codex_app_server_protocol::ThreadStatus::Idle,
            path: None,
            cwd: test_absolute_path("/tmp/orchestrator"),
            cli_version: "0.0.0".to_string(),
            source: codex_app_server_protocol::SessionSource::Unknown,
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: Some("orchestrator".to_string()),
            turns: Vec::new(),
        };

        assert_eq!(
            App::next_orchestrator_checkpoint_turn_id(None, thread_id, &base_thread),
            None
        );

        let in_progress_thread = Thread {
            turns: vec![test_turn("turn-1", TurnStatus::InProgress, Vec::new())],
            ..base_thread.clone()
        };
        assert_eq!(
            App::next_orchestrator_checkpoint_turn_id(None, thread_id, &in_progress_thread),
            None
        );

        let completed_thread = Thread {
            turns: vec![test_turn("turn-1", TurnStatus::Completed, Vec::new())],
            ..base_thread.clone()
        };
        assert_eq!(
            App::next_orchestrator_checkpoint_turn_id(None, thread_id, &completed_thread),
            Some("turn-1")
        );

        let mut binding = OracleThreadBinding::default();
        binding
            .last_checkpoint_turn_ids
            .insert(thread_id, "turn-1".to_string());
        assert_eq!(
            App::next_orchestrator_checkpoint_turn_id(Some(&binding), thread_id, &completed_thread),
            None
        );

        let failed_thread = Thread {
            turns: vec![
                test_turn("turn-1", TurnStatus::Completed, Vec::new()),
                test_turn("turn-2", TurnStatus::Failed, Vec::new()),
            ],
            ..base_thread
        };
        assert_eq!(
            App::next_orchestrator_checkpoint_turn_id(Some(&binding), thread_id, &failed_thread),
            Some("turn-2")
        );

        binding
            .pending_checkpoint_turn_ids
            .insert(thread_id, "turn-2".to_string());
        assert_eq!(
            App::orchestrator_checkpoint_retry_pending_turn(
                Some(&binding),
                thread_id,
                &completed_thread
            ),
            Some(("turn-2", "turn-1"))
        );
        assert_eq!(
            App::orchestrator_checkpoint_retry_pending_turn(
                Some(&binding),
                thread_id,
                &failed_thread
            ),
            None
        );
    }

    #[test]
    fn oracle_delegate_user_message_prefixes_objective_status() {
        assert_eq!(
            App::oracle_delegate_user_message(""),
            "Oracle delegated work to the orchestrator."
        );
        assert_eq!(
            App::oracle_delegate_user_message("Oracle delegated work to the orchestrator."),
            "Oracle delegated work to the orchestrator."
        );
        assert_eq!(
            App::oracle_delegate_user_message("Milestone complete."),
            "Oracle delegated work to the orchestrator.\n\nOracle note:\nMilestone complete."
        );
    }

    #[tokio::test]
    async fn oracle_missing_required_control_is_detected_and_repair_only_exhausts_after_retry()
    -> Result<()> {
        let initial = OracleRunResult {
            run_id: "oracle-run-repair-0".to_string(),
            oracle_thread_id: ThreadId::new(),
            kind: OracleRequestKind::UserTurn,
            requested_slug: "oracle-session-a".to_string(),
            session_id: "oracle-session-a".to_string(),
            requested_prompt: "Please tell the orchestrator to compute 2+2 and report back."
                .to_string(),
            source_user_text: Some(
                "Please tell the orchestrator to compute 2+2 and report back.".to_string(),
            ),
            files: Vec::new(),
            requires_control: true,
            repair_attempt: 0,
            response: parse_oracle_response(
                "I’m handing this to the orchestrator now and will report back.",
            ),
        };
        assert!(App::oracle_run_missing_required_control(&initial));
        assert!(!App::oracle_control_repair_exhausted(&initial));

        let retried = OracleRunResult {
            repair_attempt: 1,
            ..initial.clone()
        };
        assert!(App::oracle_run_missing_required_control(&retried));
        assert!(App::oracle_control_repair_exhausted(&retried));

        let direct_reply = OracleRunResult {
            requires_control: false,
            ..initial
        };
        assert!(!App::oracle_run_missing_required_control(&direct_reply));
        let app = make_test_app().await;
        assert_eq!(app.oracle_control_repair_message(&direct_reply), None);

        let checkpoint = OracleRunResult {
            kind: OracleRequestKind::Checkpoint,
            requested_prompt: "Checkpoint prompt".to_string(),
            source_user_text: None,
            requires_control: true,
            ..retried
        };
        assert!(App::oracle_run_missing_required_control(&checkpoint));
        assert!(App::oracle_control_repair_exhausted(&checkpoint));
        assert!(app.oracle_control_repair_message(&checkpoint).is_some());
        Ok(())
    }

    #[tokio::test]
    async fn oracle_control_metadata_is_required_for_any_control_block() -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 3,
            ..Default::default()
        });
        let result = OracleRunResult {
            run_id: "oracle-run-metadata".to_string(),
            oracle_thread_id,
            kind: OracleRequestKind::UserTurn,
            requested_slug: "oracle-session-a".to_string(),
            session_id: "oracle-session-a".to_string(),
            requested_prompt: "Continue the workflow.".to_string(),
            source_user_text: Some("Continue the workflow.".to_string()),
            files: Vec::new(),
            requires_control: false,
            repair_attempt: 0,
            response: parse_fenced_oracle_response(
                r#"{"op":"checkpoint","workflow_id":"oracle-routing","workflow_version":3,"message_for_user":"Milestone complete.","summary":"Milestone complete.","status":"running"}"#,
            ),
        };

        assert_eq!(
            app.oracle_control_repair_message(&result),
            Some(format!(
                "Oracle control block must include `schema_version: {ORACLE_CONTROL_SCHEMA_VERSION}`."
            ))
        );
        Ok(())
    }

    #[tokio::test]
    async fn oracle_control_workflow_version_must_be_positive() -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 3,
            ..Default::default()
        });
        let result = OracleRunResult {
            run_id: "oracle-run-version-zero".to_string(),
            oracle_thread_id,
            kind: OracleRequestKind::UserTurn,
            requested_slug: "oracle-session-version-zero".to_string(),
            session_id: "oracle-session-version-zero".to_string(),
            requested_prompt: "Continue the workflow.".to_string(),
            source_user_text: Some("Continue the workflow.".to_string()),
            files: Vec::new(),
            requires_control: true,
            repair_attempt: 0,
            response: parse_fenced_oracle_response(
                r#"{"schema_version":1,"op_id":"op-zero-version","idempotency_key":"idem-zero-version","op":"handoff","workflow_id":"oracle-routing","workflow_version":0,"message":"Continue"}"#,
            ),
        };

        assert_eq!(
            app.oracle_control_repair_message(&result),
            Some(
                "Oracle control block must include a positive `workflow_version` starting at 1."
                    .to_string()
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn first_oracle_handoff_requires_explicit_workflow_metadata() -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let result = OracleRunResult {
            run_id: "oracle-run-first-handoff".to_string(),
            oracle_thread_id,
            kind: OracleRequestKind::UserTurn,
            requested_slug: "oracle-session-first-handoff".to_string(),
            session_id: "oracle-session-first-handoff".to_string(),
            requested_prompt: "Use the orchestrator for this task.".to_string(),
            source_user_text: Some("Use the orchestrator for this task.".to_string()),
            files: Vec::new(),
            requires_control: true,
            repair_attempt: 0,
            response: parse_fenced_oracle_response(
                r#"{"schema_version":1,"op_id":"op-first-handoff","idempotency_key":"idem-first-handoff","op":"handoff","message":"Run the task","summary":"Need orchestrator execution"}"#,
            ),
        };

        assert_eq!(
            app.oracle_control_repair_message(&result),
            Some(
                "Oracle control block must include an explicit `workflow_id` when starting the first workflow handoff."
                    .to_string()
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn terminal_workflow_reply_without_workflow_id_detaches_old_workflow() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing-old".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Complete,
            version: 4,
            summary: Some("Previous milestone complete".to_string()),
            ..Default::default()
        });
        app.reopen_active_oracle_workflow_for_user_turn();
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.pending_turn_id = Some("oracle-turn-terminal-reply".to_string());
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-terminal-reply".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-terminal-reply".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-terminal-reply".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "Start a fresh direct chat turn.".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }
        app.oracle_state.active_run_id = Some("oracle-run-terminal-reply".to_string());
        app.persist_active_oracle_binding();

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-terminal-reply".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "Start a fresh direct chat turn.".to_string(),
                source_user_text: Some("Start a fresh direct chat turn.".to_string()),
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_fenced_oracle_response(
                    r#"{"schema_version":1,"op_id":"op-direct-reply","idempotency_key":"idem-direct-reply","op":"reply","message_for_user":"Fresh direct chat reply."}"#,
                ),
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.workflow, None);
        Ok(())
    }

    #[tokio::test]
    async fn terminal_workflow_reply_with_new_workflow_id_does_not_attach_or_revive_workflow()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing-old".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Complete,
            version: 4,
            summary: Some("Previous milestone complete".to_string()),
            ..Default::default()
        });
        app.reopen_active_oracle_workflow_for_user_turn();
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.pending_turn_id = Some("oracle-turn-terminal-reply-new-id".to_string());
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-terminal-reply-new-id".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-terminal-reply-new-id".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-terminal-reply-new-id".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "Stay in direct chat even if Oracle suggests a workflow id."
                            .to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }
        app.oracle_state.active_run_id = Some("oracle-run-terminal-reply-new-id".to_string());
        app.persist_active_oracle_binding();

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-terminal-reply-new-id".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt:
                    "Stay in direct chat even if Oracle suggests a workflow id.".to_string(),
                source_user_text: Some(
                    "Stay in direct chat even if Oracle suggests a workflow id.".to_string(),
                ),
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_fenced_oracle_response(
                    r#"{"schema_version":1,"op_id":"op-direct-reply-new-id","idempotency_key":"idem-direct-reply-new-id","op":"reply","workflow_id":"oracle-routing-new","workflow_version":1,"message_for_user":"This is still a direct reply."}"#,
                ),
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.workflow, None);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_replacement_workflow_does_not_attach_before_repair() -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_repo =
            find_oracle_repo(app.chat_widget.config_ref().cwd.as_path()).expect("oracle repo");
        app.oracle_broker = Some((oracle_repo, OracleBrokerClient::new_test_client()));
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing-old".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Complete,
            version: 4,
            summary: Some("Previous milestone complete".to_string()),
            ..Default::default()
        });
        app.reopen_active_oracle_workflow_for_user_turn();
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.pending_turn_id = Some("oracle-turn-invalid-replacement".to_string());
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-invalid-replacement".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-invalid-replacement".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-invalid-replacement".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "Start a new workflow only if the control block is valid."
                            .to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }
        app.oracle_state.active_run_id = Some("oracle-run-invalid-replacement".to_string());
        app.persist_active_oracle_binding();

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-invalid-replacement".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt:
                    "Start a new workflow only if the control block is valid.".to_string(),
                source_user_text: Some(
                    "Start a new workflow only if the control block is valid.".to_string(),
                ),
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_fenced_oracle_response(
                    r#"{"op":"checkpoint","workflow_id":"oracle-routing-new","workflow_version":1,"message_for_user":"Milestone complete.","summary":"Milestone complete.","status":"running"}"#,
                ),
            },
        )
        .await?;

        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn)
        );
        let workflow = app
            .oracle_state
            .workflow
            .as_ref()
            .expect("existing workflow");
        assert_eq!(workflow.workflow_id, "oracle-routing-old");
        assert_eq!(workflow.status, OracleWorkflowStatus::Complete);
        Ok(())
    }

    #[tokio::test]
    async fn oracle_handoff_creates_workflow_and_submits_orchestrator_reminder() -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.oracle_state.active_run_id = Some("oracle-run-handoff".to_string());
        app.persist_active_oracle_binding();

        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-handoff".to_string(),
                oracle_thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "User turn prompt".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_fenced_oracle_response(
                    r#"{"schema_version":1,"op_id":"op-handoff-creates-workflow","idempotency_key":"idem-handoff-creates-workflow","op":"handoff","message":"Audit the diff and summarize risks.","message_for_user":"Implementation handoff ready.","workflow_id":"oracle-routing","workflow_version":2,"objective":"Ship the Oracle workflow refactor","summary":"Implementation plan ready"}"#,
                ),
            },
        )
        .await?;

        let orchestrator_thread_id = app
            .oracle_state
            .orchestrator_thread_id
            .expect("orchestrator thread");
        let workflow = app.oracle_state.workflow.clone().expect("workflow binding");
        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");

        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOrchestrator
        );
        assert_eq!(workflow.workflow_id, "oracle-routing");
        assert_eq!(workflow.version, 2);
        assert_eq!(
            workflow.objective.as_deref(),
            Some("Ship the Oracle workflow refactor")
        );
        assert_eq!(
            workflow.summary.as_deref(),
            Some("Implementation plan ready")
        );
        assert_eq!(workflow.status, OracleWorkflowStatus::Running);
        assert_eq!(
            workflow.orchestrator_thread_id,
            Some(orchestrator_thread_id)
        );
        assert!(
            binding.participants.is_empty(),
            "workflow handoff should not persist routed worker state"
        );
        assert!(app.is_hidden_oracle_thread(orchestrator_thread_id));
        assert!(app.agent_navigation.get(&orchestrator_thread_id).is_none());

        let routed_op = app
            .oracle_destination_task_op(
                oracle_thread_id,
                orchestrator_thread_id,
                ORACLE_ORCHESTRATOR_DESTINATION,
                Some("orchestrator"),
                "Audit the diff and summarize risks.".to_string(),
                None,
            )
            .await;
        let AppCommandView::UserTurn { items, .. } = routed_op.view() else {
            panic!("expected orchestrator op to be a user turn");
        };
        let routed_text = items
            .iter()
            .filter_map(|item| match item {
                UserInput::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(routed_text.contains("Oracle workflow contract"));
        assert!(routed_text.contains("oracle-routing"));
        assert!(routed_text.contains("version 2"));
        assert!(routed_text.contains("Audit the diff and summarize risks."));
        Ok(())
    }

    #[tokio::test]
    async fn completed_workflow_allows_new_oracle_workflow_id_to_replace_it() {
        let mut app = make_test_app().await;
        let _oracle_thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            status: OracleWorkflowStatus::Complete,
            version: 4,
            summary: Some("previous milestone complete".to_string()),
            ..Default::default()
        });

        let stale_reason = app.active_oracle_workflow_stale_reason(Some(&OracleControlDirective {
            workflow_id: Some("oracle-routing-2".to_string()),
            workflow_version: Some(1),
            ..Default::default()
        }));
        assert!(stale_reason.is_none());

        let workflow = app.ensure_active_oracle_workflow(
            Some("oracle-routing-2".to_string()),
            Some("new objective".to_string()),
            Some("new summary".to_string()),
            Some(1),
        );
        assert_eq!(workflow.workflow_id, "oracle-routing-2");
        assert_eq!(workflow.version, 1);
        assert_eq!(workflow.status, OracleWorkflowStatus::Running);
        assert_eq!(workflow.objective.as_deref(), Some("new objective"));
        assert_eq!(workflow.summary.as_deref(), Some("new summary"));
    }

    #[tokio::test]
    async fn persist_active_oracle_binding_preserves_participants() {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        if let Some(binding) = app.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.participants.insert(
                "worker:test".to_string(),
                OracleRoutingParticipant {
                    address: "worker:test".to_string(),
                    thread_id: Some(ThreadId::new()),
                    title: Some("worker".to_string()),
                    kind: Some("worker".to_string()),
                    role: Some("worker".to_string()),
                    visibility: OracleParticipantVisibility::Hidden,
                    owned_by_oracle: true,
                    route_completions: true,
                    route_closures: true,
                    last_task: Some("keep state".to_string()),
                },
            );
        }

        app.activate_oracle_binding(oracle_thread_id);
        app.oracle_state.last_status = Some("updated".to_string());
        app.persist_active_oracle_binding();

        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert!(binding.participants.contains_key("worker:test"));
    }

    #[tokio::test]
    async fn oracle_delegate_status_replays_as_system_event_not_assistant_message() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.pending_turn_id = Some("oracle-turn-1".to_string());
        app.persist_active_oracle_binding();
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-1".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-1".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-1".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "ship the feature".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.oracle_state.active_run_id = Some("oracle-run-delegate".to_string());
        app.persist_active_oracle_binding();
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-delegate".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "User turn prompt".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_fenced_oracle_response(
                    r#"{"schema_version":1,"op_id":"op-delegate-status-replay","idempotency_key":"idem-delegate-status-replay","op":"handoff","message":"Audit the diff and summarize risks.","message_for_user":"Implementation handoff ready.","workflow_id":"oracle-routing","workflow_version":2,"objective":"Ship the Oracle workflow refactor","summary":"Implementation plan ready"}"#,
                ),
            },
        )
        .await?;

        let snapshot = {
            let channel = app
                .thread_event_channels
                .get(&thread_id)
                .expect("oracle thread channel");
            let store = channel.store.lock().await;
            let turn = store
                .turns
                .iter()
                .find(|turn| turn.id == "oracle-turn-1")
                .expect("oracle turn");
            assert_eq!(turn.status, TurnStatus::Completed);
            assert!(!turn.items.iter().any(|item| matches!(
                item,
                ThreadItem::AgentMessage { text, .. }
                    if text.contains("Oracle delegated work to the orchestrator.")
            )));
            assert!(store.buffer.iter().any(|event| matches!(
                event,
                ThreadBufferedEvent::OracleWorkflowEvent(OracleWorkflowThreadEvent { title, details })
                    if title == "Oracle delegated work to the orchestrator."
                        && details.iter().any(|detail| detail == "Workflow: oracle-routing v2")
                        && details.iter().any(|detail| detail == "Note: Implementation handoff ready.")
            )));
            store.snapshot()
        };

        while app_event_rx.try_recv().is_ok() {}
        app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);

        let rendered_cells = std::iter::from_fn(|| app_event_rx.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => {
                    Some(lines_to_single_string(&cell.display_lines(/*width*/ 120)))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(rendered_cells.iter().any(|cell| {
            cell.contains("• Oracle delegated work to the orchestrator.")
                && cell.contains("Workflow: oracle-routing v2")
                && cell.contains("Note: Implementation handoff ready.")
        }));
        Ok(())
    }

    #[tokio::test]
    async fn active_oracle_workflow_event_renders_before_followup_reply() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let session = app.oracle_thread_session(thread_id);
        app.activate_thread_channel(thread_id).await;
        app.chat_widget.handle_thread_session(session);
        while app_event_rx.try_recv().is_ok() {}

        app.append_oracle_workflow_event(
            thread_id,
            OracleWorkflowThreadEvent {
                title: "Oracle delegated work to the orchestrator.".to_string(),
                details: vec!["Workflow: oracle-wf-test v1".to_string()],
            },
        )
        .await;
        app.append_oracle_agent_turn(
            thread_id,
            "I received the orchestrator response: orchestrator test complete: 4.",
        )
        .await;

        let rendered_cells = std::iter::from_fn(|| app_event_rx.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => {
                    Some(lines_to_single_string(&cell.display_lines(/*width*/ 120)))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        let workflow_index = rendered_cells
            .iter()
            .position(|cell| cell.contains("Oracle delegated work to the orchestrator."))
            .expect("workflow event should render");
        let reply_index = rendered_cells
            .iter()
            .position(|cell| {
                cell.contains(
                    "I received the orchestrator response: orchestrator test complete: 4.",
                )
            })
            .expect("oracle reply should render");
        assert!(workflow_index < reply_index);
    }

    #[tokio::test]
    async fn oracle_workflow_event_replays_before_followup_reply_after_thread_switch() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let session = app.oracle_thread_session(thread_id);
        app.activate_thread_channel(thread_id).await;
        app.chat_widget.handle_thread_session(session);
        while app_event_rx.try_recv().is_ok() {}

        app.begin_oracle_user_turn(
            thread_id,
            &[UserInput::Text {
                text: "Demonstrate the orchestrator handoff.".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .await;
        app.append_oracle_workflow_event(
            thread_id,
            OracleWorkflowThreadEvent {
                title: "Oracle delegated work to the orchestrator.".to_string(),
                details: vec!["Workflow: demo-orchestrator-smoke v1".to_string()],
            },
        )
        .await;
        app.complete_pending_oracle_turn(
            thread_id,
            "The orchestrator completed the task successfully. Result: orchestrator smoke test complete.",
        )
        .await;

        let snapshot = {
            let channel = app
                .thread_event_channels
                .get(&thread_id)
                .expect("oracle thread channel");
            let store = channel.store.lock().await;
            assert_eq!(store.replay_entries.len(), 3);
            store.snapshot()
        };

        while app_event_rx.try_recv().is_ok() {}
        app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);

        let rendered_cells = std::iter::from_fn(|| app_event_rx.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => {
                    Some(lines_to_single_string(&cell.display_lines(/*width*/ 140)))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        let user_index = rendered_cells
            .iter()
            .position(|cell| cell.contains("Demonstrate the orchestrator handoff."))
            .expect("oracle user message should replay");
        let workflow_index = rendered_cells
            .iter()
            .position(|cell| cell.contains("Oracle delegated work to the orchestrator."))
            .expect("workflow event should replay");
        let reply_index = rendered_cells
            .iter()
            .position(|cell| {
                cell.contains(
                    "The orchestrator completed the task successfully. Result: orchestrator smoke test complete.",
                )
            })
            .expect("oracle reply should replay");

        assert!(user_index < workflow_index);
        assert!(workflow_index < reply_index);
    }

    #[tokio::test]
    async fn refreshed_oracle_snapshot_keeps_workflow_event_before_completed_reply() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let session = app.oracle_thread_session(thread_id);
        app.activate_thread_channel(thread_id).await;
        app.chat_widget.handle_thread_session(session.clone());
        while app_event_rx.try_recv().is_ok() {}

        app.begin_oracle_user_turn(
            thread_id,
            &[UserInput::Text {
                text: "Ask the orchestrator to compute 2+2.".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .await;
        let turn_id = app
            .oracle_state
            .pending_turn_id
            .clone()
            .expect("pending oracle turn");
        app.append_oracle_workflow_event(
            thread_id,
            OracleWorkflowThreadEvent {
                title: "Oracle delegated work to the orchestrator.".to_string(),
                details: vec!["Workflow: oracle-wf-refresh v1".to_string()],
            },
        )
        .await;

        let mut snapshot = {
            let channel = app
                .thread_event_channels
                .get(&thread_id)
                .expect("oracle thread channel");
            let store = channel.store.lock().await;
            store.snapshot()
        };
        let refreshed_turn = Turn {
            id: turn_id,
            items: vec![
                ThreadItem::UserMessage {
                    id: "oracle-user-refresh".to_string(),
                    content: vec![AppServerUserInput::Text {
                        text: "Ask the orchestrator to compute 2+2.".to_string(),
                        text_elements: Vec::new(),
                    }],
                },
                ThreadItem::AgentMessage {
                    id: "oracle-assistant-refresh".to_string(),
                    text: "The orchestrator responded with 4.".to_string(),
                    phase: None,
                    memory_citation: None,
                },
            ],
            status: TurnStatus::Completed,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        };

        app.apply_refreshed_snapshot_thread(
            thread_id,
            AppServerStartedThread {
                session,
                turns: vec![refreshed_turn],
            },
            &mut snapshot,
        )
        .await;

        while app_event_rx.try_recv().is_ok() {}
        app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);

        let rendered_cells = std::iter::from_fn(|| app_event_rx.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => {
                    Some(lines_to_single_string(&cell.display_lines(/*width*/ 140)))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        let user_index = rendered_cells
            .iter()
            .position(|cell| cell.contains("Ask the orchestrator to compute 2+2."))
            .expect("oracle user message should replay");
        let workflow_index = rendered_cells
            .iter()
            .position(|cell| cell.contains("Oracle delegated work to the orchestrator."))
            .expect("workflow event should replay");
        let reply_index = rendered_cells
            .iter()
            .position(|cell| cell.contains("The orchestrator responded with 4."))
            .expect("oracle reply should replay");

        assert!(user_index < workflow_index);
        assert!(workflow_index < reply_index);
    }
    #[tokio::test]
    async fn oracle_pending_turn_completion_renders_when_widget_thread_outpaces_active_thread() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let session = app.oracle_thread_session(thread_id);
        app.chat_widget.handle_thread_session(session);
        app.active_thread_id = Some(thread_id);
        while app_event_rx.try_recv().is_ok() {}

        app.begin_oracle_user_turn(
            thread_id,
            &[UserInput::Text {
                text: "Resume after interrupt.".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .await;
        while app_event_rx.try_recv().is_ok() {}

        app.active_thread_id = Some(ThreadId::new());
        app.complete_pending_oracle_turn(thread_id, "RESUME_AFTER_INTERRUPT")
            .await;

        let rendered_cells = std::iter::from_fn(|| app_event_rx.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => {
                    Some(lines_to_single_string(&cell.display_lines(/*width*/ 140)))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(
            rendered_cells
                .iter()
                .any(|cell| cell.contains("RESUME_AFTER_INTERRUPT")),
            "the resumed oracle reply should render even if active_thread_id lags behind the widget"
        );
    }

    #[tokio::test]
    async fn oracle_destination_task_uses_destination_thread_model_not_oracle_display_label()
    -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.model = OracleModelPreset::Thinking;
        app.sync_oracle_thread_session(oracle_thread_id).await;

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let orchestrator_thread_id = app.ensure_orchestrator_thread(&mut app_server).await?;
        let expected_model = {
            let channel = app
                .thread_event_channels
                .get(&orchestrator_thread_id)
                .expect("orchestrator thread channel");
            let store = channel.store.lock().await;
            store
                .session
                .as_ref()
                .map(|session| session.model.clone())
                .expect("orchestrator session model")
        };

        let routed_op = app
            .oracle_destination_task_op(
                oracle_thread_id,
                orchestrator_thread_id,
                ORACLE_ORCHESTRATOR_DESTINATION,
                Some("orchestrator"),
                "Reply with ORCH-OK.".to_string(),
                Some(orchestrator_developer_instructions()),
            )
            .await;
        let AppCommandView::UserTurn {
            model,
            collaboration_mode,
            ..
        } = routed_op.view()
        else {
            panic!("expected orchestrator op to be a user turn");
        };

        assert_eq!(*model, expected_model);
        assert_eq!(
            collaboration_mode
                .as_ref()
                .map(|mode| mode.model().to_string())
                .as_deref(),
            Some(expected_model.as_str())
        );
        assert_ne!(model, "requested gpt-5.5 (Thinking 5.5)");
        Ok(())
    }

    #[tokio::test]
    async fn stale_oracle_workflow_handoff_is_ignored_safely() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 4,
            objective: Some("Ship the Oracle workflow refactor".to_string()),
            ..Default::default()
        });
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.pending_turn_id = Some("oracle-turn-1".to_string());
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-1".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-1".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-1".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "ship the feature".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.oracle_state.active_run_id = Some("oracle-run-stale".to_string());
        app.persist_active_oracle_binding();
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-stale".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "User turn prompt".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_fenced_oracle_response(
                    r#"{"schema_version":1,"op_id":"op-stale-version","idempotency_key":"idem-stale-version","op":"handoff","message":"Outdated plan","workflow_id":"oracle-routing","workflow_version":2}"#,
                ),
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(
            app.oracle_state
                .workflow
                .as_ref()
                .map(|workflow| workflow.version),
            Some(4)
        );
        assert_eq!(app.oracle_state.orchestrator_thread_id, None);
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        let turn = store
            .turns
            .iter()
            .find(|turn| turn.id == "oracle-turn-1")
            .expect("oracle turn");
        assert_eq!(turn.status, TurnStatus::Completed);
        assert!(store.buffer.iter().any(|event| matches!(
            event,
            ThreadBufferedEvent::OracleWorkflowEvent(OracleWorkflowThreadEvent { title, .. })
                if title.contains("workflow version")
        )));
        Ok(())
    }

    #[tokio::test]
    async fn completed_oracle_workflow_stays_terminal_before_new_user_turn_starts() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Complete,
            version: 1,
            objective: Some("Ship the Oracle workflow refactor".to_string()),
            summary: Some("Milestone complete".to_string()),
            last_blocker: Some("done".to_string()),
            orchestrator_thread_id: Some(orchestrator_thread_id),
            ..Default::default()
        });
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.reopen_active_oracle_workflow_for_user_turn();
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.pending_turn_id = Some("oracle-turn-2".to_string());
        app.persist_active_oracle_binding();
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-2".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-2".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-2".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "continue supervising".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }

        let workflow = app
            .oracle_state
            .workflow
            .as_ref()
            .expect("workflow retained");
        assert_eq!(workflow.status, OracleWorkflowStatus::Complete);
        assert_eq!(workflow.version, 1);
        assert_eq!(
            workflow.objective.as_deref(),
            Some("Ship the Oracle workflow refactor")
        );
        assert_eq!(workflow.summary.as_deref(), Some("Milestone complete"));
        assert_eq!(workflow.last_blocker.as_deref(), Some("done"));
        assert!(app.oracle_state.accept_user_turn_workflow_replacement);
        let prompt = build_user_turn_prompt(&app.oracle_state, "continue supervising", false);
        assert!(prompt.contains("Mode: direct_chat"));
        assert!(prompt.contains("No workflow is currently attached."));
        assert!(!prompt.contains("workflow_id: oracle-routing"));

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.oracle_state.active_run_id = Some("oracle-run-stale-finish".to_string());
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-stale-finish".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-b".to_string(),
                session_id: "oracle-session-b".to_string(),
                requested_prompt: "continue supervising".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_fenced_oracle_response(
                    r#"{"schema_version":1,"op_id":"op-stale-finish","idempotency_key":"idem-stale-finish","op":"finish","message_for_user":"stale completion","workflow_id":"oracle-routing","workflow_version":1}"#,
                ),
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(
            app.oracle_state
                .workflow
                .as_ref()
                .map(|workflow| (workflow.status, workflow.version)),
            Some((OracleWorkflowStatus::Complete, 1))
        );
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        let turn = store
            .turns
            .iter()
            .find(|turn| turn.id == "oracle-turn-2")
            .expect("oracle turn");
        assert_eq!(turn.status, TurnStatus::Completed);
        assert!(!turn.items.iter().any(|item| matches!(
            item,
            ThreadItem::AgentMessage { text, .. } if text == "stale completion"
        )));
        assert!(store.buffer.iter().any(|event| matches!(
            event,
            ThreadBufferedEvent::OracleWorkflowEvent(OracleWorkflowThreadEvent { title, .. })
                if title.contains("terminal workflow")
        )));
        Ok(())
    }

    #[tokio::test]
    async fn completed_oracle_workflow_rejects_late_same_workflow_handoff() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Complete,
            version: 1,
            objective: Some("Ship the Oracle workflow refactor".to_string()),
            summary: Some("Milestone complete".to_string()),
            ..Default::default()
        });
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.pending_turn_id = Some("oracle-turn-terminal-handoff".to_string());
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-terminal-handoff".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-terminal-handoff".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-terminal-handoff".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "continue supervising".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.oracle_state.active_run_id = Some("oracle-run-terminal-handoff".to_string());
        app.persist_active_oracle_binding();
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-terminal-handoff".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-terminal-handoff".to_string(),
                session_id: "oracle-session-terminal-handoff".to_string(),
                requested_prompt: "continue supervising".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_fenced_oracle_response(
                    r#"{"schema_version":1,"op_id":"op-stale-handoff","idempotency_key":"idem-stale-handoff","op":"handoff","message":"Outdated plan","workflow_id":"oracle-routing","workflow_version":1}"#,
                ),
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.orchestrator_thread_id, None);
        assert_eq!(
            app.oracle_state
                .workflow
                .as_ref()
                .map(|workflow| (workflow.status, workflow.version)),
            Some((OracleWorkflowStatus::Complete, 1))
        );
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        let turn = store
            .turns
            .iter()
            .find(|turn| turn.id == "oracle-turn-terminal-handoff")
            .expect("oracle turn");
        assert_eq!(turn.status, TurnStatus::Completed);
        assert!(store.buffer.iter().any(|event| matches!(
            event,
            ThreadBufferedEvent::OracleWorkflowEvent(OracleWorkflowThreadEvent { title, .. })
                if title.contains("terminal workflow")
        )));
        Ok(())
    }

    #[tokio::test]
    async fn completed_oracle_workflow_accepts_new_workflow_id_on_next_user_turn() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let old_orchestrator_thread_id = ThreadId::new();
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing-old".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Complete,
            version: 4,
            objective: Some("Previous workflow".to_string()),
            summary: Some("Previous milestone complete".to_string()),
            orchestrator_thread_id: Some(old_orchestrator_thread_id),
            ..Default::default()
        });
        app.oracle_state.orchestrator_thread_id = Some(old_orchestrator_thread_id);
        app.reopen_active_oracle_workflow_for_user_turn();
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.pending_turn_id = Some("oracle-turn-3".to_string());
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-3".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-3".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-3".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "start a new workflow".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }
        assert!(app.oracle_state.accept_user_turn_workflow_replacement);
        app.oracle_state.active_checkpoint_thread_id = Some(old_orchestrator_thread_id);
        app.oracle_state.active_checkpoint_turn_id = Some("old-checkpoint-review".to_string());
        if let Some(binding) = app.oracle_state.bindings.get_mut(&thread_id) {
            binding.pending_checkpoint_threads = vec![old_orchestrator_thread_id];
            binding
                .pending_checkpoint_versions
                .insert(old_orchestrator_thread_id, 4);
            binding
                .pending_checkpoint_turn_ids
                .insert(old_orchestrator_thread_id, "old-turn-1".to_string());
            binding
                .last_checkpoint_turn_ids
                .insert(old_orchestrator_thread_id, "old-turn-reviewed".to_string());
            binding.active_checkpoint_thread_id = Some(old_orchestrator_thread_id);
            binding.active_checkpoint_turn_id = Some("old-checkpoint-review".to_string());
        }

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.oracle_state.active_run_id = Some("oracle-run-new-workflow".to_string());
        app.persist_active_oracle_binding();
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-new-workflow".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-c".to_string(),
                session_id: "oracle-session-c".to_string(),
                requested_prompt: "start a new workflow".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_fenced_oracle_response(
                    r#"{"schema_version":1,"op_id":"op-completed-workflow-replacement","idempotency_key":"idem-completed-workflow-replacement","op":"handoff","workflow_id":"oracle-routing-new","workflow_version":1,"objective":"Handle a fresh task","summary":"New workflow summary","message":"Run the new task","status":"running"}"#,
                ),
            },
        )
        .await?;

        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOrchestrator
        );
        assert!(!app.oracle_state.accept_user_turn_workflow_replacement);
        let workflow = app.oracle_state.workflow.as_ref().expect("new workflow");
        assert_eq!(workflow.workflow_id, "oracle-routing-new");
        assert_eq!(workflow.version, 1);
        assert_eq!(workflow.summary.as_deref(), Some("New workflow summary"));
        assert_eq!(app.oracle_state.active_checkpoint_thread_id, None);
        assert_eq!(app.oracle_state.active_checkpoint_turn_id, None);
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert!(binding.pending_checkpoint_threads.is_empty());
        assert!(binding.pending_checkpoint_versions.is_empty());
        assert!(binding.pending_checkpoint_turn_ids.is_empty());
        assert!(binding.last_checkpoint_turn_ids.is_empty());
        assert_eq!(binding.active_checkpoint_thread_id, None);
        assert_eq!(binding.active_checkpoint_turn_id, None);
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert!(!store.buffer.iter().any(|event| matches!(
            event,
            ThreadBufferedEvent::OracleWorkflowEvent(OracleWorkflowThreadEvent { title, .. })
                if title.contains("Ignoring the stale response")
        )));
        Ok(())
    }

    #[tokio::test]
    async fn completed_oracle_workflow_without_explicit_id_requests_repair() -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_repo =
            find_oracle_repo(app.chat_widget.config_ref().cwd.as_path()).expect("oracle repo");
        app.oracle_broker = Some((oracle_repo, OracleBrokerClient::new_test_client()));
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing-old".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Complete,
            version: 4,
            objective: Some("Previous workflow".to_string()),
            summary: Some("Previous milestone complete".to_string()),
            ..Default::default()
        });
        app.reopen_active_oracle_workflow_for_user_turn();
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.pending_turn_id = Some("oracle-turn-4".to_string());
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-4".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-4".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-4".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "start a fresh workflow without naming it".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }
        assert!(app.oracle_state.accept_user_turn_workflow_replacement);

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.oracle_state.active_run_id = Some("oracle-run-fresh-workflow".to_string());
        app.persist_active_oracle_binding();
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-fresh-workflow".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-d".to_string(),
                session_id: "oracle-session-d".to_string(),
                requested_prompt: "start a fresh workflow without naming it".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_fenced_oracle_response(
                    r#"{"schema_version":1,"op_id":"op-completed-workflow-fresh-default","idempotency_key":"idem-completed-workflow-fresh-default","op":"handoff","objective":"Handle a fresh task","summary":"New workflow summary","message":"Run the new task","status":"running"}"#,
                ),
            },
        )
        .await?;

        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn)
        );
        let workflow = app
            .oracle_state
            .workflow
            .as_ref()
            .expect("workflow retained");
        assert_eq!(workflow.workflow_id, "oracle-routing-old");
        assert_eq!(workflow.status, OracleWorkflowStatus::Complete);
        assert_eq!(workflow.version, 4);
        assert_eq!(app.oracle_state.orchestrator_thread_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn oracle_reply_in_chat_mode_leaves_workflow_unset() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.pending_turn_id = Some("oracle-turn-1".to_string());
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-1".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-1".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-1".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "hello oracle".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.oracle_state.active_run_id = Some("oracle-run-chat".to_string());
        app.persist_active_oracle_binding();
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-chat".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-chat".to_string(),
                session_id: "oracle-session-chat".to_string(),
                requested_prompt: "hello oracle".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_oracle_response("Hello back."),
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.workflow, None);
        assert_eq!(app.oracle_state.orchestrator_thread_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn oracle_reply_during_active_workflow_preserves_blocker_state() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 2,
            objective: Some("Finish the task".to_string()),
            summary: Some("Work in progress".to_string()),
            ..Default::default()
        });
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.pending_turn_id = Some("oracle-turn-blocker".to_string());
        app.oracle_state.active_run_id = Some("oracle-run-blocker".to_string());
        app.persist_active_oracle_binding();
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-blocker".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-blocker".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-blocker".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "What is blocking you?".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-blocker".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-blocker".to_string(),
                session_id: "oracle-session-blocker".to_string(),
                requested_prompt: "What is blocking you?".to_string(),
                source_user_text: Some("What is blocking you?".to_string()),
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: parse_fenced_oracle_response(
                    r#"{"schema_version":1,"op_id":"op-reply-blocker","idempotency_key":"idem-reply-blocker","op":"reply","workflow_id":"oracle-routing","workflow_version":2,"message_for_user":"Need the user to confirm the target path.","summary":"Waiting on the user to confirm the target path."}"#,
                ),
            },
        )
        .await?;

        let workflow = app.oracle_state.workflow.as_ref().expect("workflow");
        assert_eq!(workflow.status, OracleWorkflowStatus::NeedsHuman);
        assert_eq!(
            workflow.last_blocker.as_deref(),
            Some("Need the user to confirm the target path.")
        );
        assert_eq!(
            workflow.summary.as_deref(),
            Some("Waiting on the user to confirm the target path.")
        );
        assert_eq!(workflow.last_checkpoint.as_deref(), None);
        Ok(())
    }

    #[tokio::test]
    async fn oracle_reply_survives_binding_run_id_drift() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.active_run_id = Some("oracle-run-drift".to_string());
        app.oracle_state
            .inflight_runs
            .insert(thread_id, "oracle-run-drift".to_string());
        app.oracle_state.pending_turn_id = Some("oracle-turn-1".to_string());
        app.persist_active_oracle_binding();
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-1".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-1".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-1".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "hello oracle".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }

        // Simulate the stale binding drift seen in live Oracle follow-ups.
        app.oracle_state.active_run_id = None;
        if let Some(binding) = app.oracle_state.bindings.get_mut(&thread_id) {
            binding.active_run_id = None;
        }
        remember_oracle_run_remote_metadata(
            "oracle-run-drift",
            Some("remote-drift".to_string()),
            Some("Remote Drift".to_string()),
        );

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-drift".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-chat".to_string(),
                session_id: "oracle-session-chat-2".to_string(),
                requested_prompt: "hello oracle".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_oracle_response("Hello back."),
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.active_run_id, None);
        assert!(!app.oracle_state.inflight_runs.contains_key(&thread_id));
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        let turn = store
            .turns
            .iter()
            .find(|turn| turn.id == "oracle-turn-1")
            .expect("oracle turn");
        assert_eq!(turn.status, TurnStatus::Completed);
        assert!(turn.items.iter().any(|item| matches!(
            item,
            ThreadItem::AgentMessage { text, .. } if text == "Hello back."
        )));
        drop(store);
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(binding.conversation_id.as_deref(), Some("remote-drift"));
        assert_eq!(binding.remote_title.as_deref(), Some("Remote Drift"));
        Ok(())
    }

    #[tokio::test]
    async fn oracle_run_completion_rejects_conversation_identity_drift() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        app.set_oracle_thread_broker_session_id(thread_id, Some("runtime-root".to_string()));
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.active_run_id = Some("oracle-run-drift".to_string());
        app.oracle_state
            .inflight_runs
            .insert(thread_id, "oracle-run-drift".to_string());
        app.oracle_state.pending_turn_id = Some("oracle-turn-1".to_string());
        app.persist_active_oracle_binding();
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-1".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-1".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-1".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "hello oracle".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }
        remember_oracle_run_remote_metadata(
            "oracle-run-drift",
            Some("wrong-9".to_string()),
            Some("Wrong Thread".to_string()),
        );

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-drift".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-chat".to_string(),
                session_id: "oracle-session-chat-2".to_string(),
                requested_prompt: "hello oracle".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_oracle_response("Hello back."),
            },
        )
        .await?;

        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(binding.conversation_id.as_deref(), Some("attached-1"));
        assert_eq!(binding.current_session_id, None);
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        let turn = store
            .turns
            .iter()
            .find(|turn| turn.id == "oracle-turn-1")
            .expect("oracle turn");
        assert_eq!(turn.status, TurnStatus::Completed);
        assert!(turn.items.iter().any(|item| matches!(
            item,
            ThreadItem::AgentMessage { text, .. }
                if text.contains("Oracle failed: Oracle thread identity violation")
        )));
        assert!(!turn.items.iter().any(|item| matches!(
            item,
            ThreadItem::AgentMessage { text, .. } if text == "Hello back."
        )));
        Ok(())
    }

    #[tokio::test]
    async fn oracle_run_completion_rejects_missing_conversation_metadata_for_bound_thread()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        app.set_oracle_thread_broker_session_id(thread_id, Some("runtime-root".to_string()));
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.active_run_id = Some("oracle-run-missing-meta".to_string());
        app.oracle_state
            .inflight_runs
            .insert(thread_id, "oracle-run-missing-meta".to_string());
        app.oracle_state.pending_turn_id = Some("oracle-turn-1".to_string());
        app.persist_active_oracle_binding();
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-1".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-1".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-1".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "hello oracle".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }
        remember_oracle_run_remote_metadata(
            "oracle-run-missing-meta",
            None,
            Some("Attached Thread".to_string()),
        );

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-missing-meta".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-chat".to_string(),
                session_id: "oracle-session-chat-2".to_string(),
                requested_prompt: "hello oracle".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_oracle_response("Hello back."),
            },
        )
        .await?;

        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        let turn = store
            .turns
            .iter()
            .find(|turn| turn.id == "oracle-turn-1")
            .expect("oracle turn");
        assert_eq!(turn.status, TurnStatus::Completed);
        assert!(turn.items.iter().any(|item| matches!(
            item,
            ThreadItem::AgentMessage { text, .. }
                if text.contains("Oracle failed: Oracle thread identity violation")
                    && text.contains("omitted conversation metadata")
        )));
        assert!(!turn.items.iter().any(|item| matches!(
            item,
            ThreadItem::AgentMessage { text, .. } if text == "Hello back."
        )));
        Ok(())
    }

    #[tokio::test]
    async fn oracle_run_completion_accepts_missing_metadata_for_followup_anchored_run() -> Result<()>
    {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        app.set_oracle_thread_broker_session_id(thread_id, Some("runtime-root".to_string()));
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.active_run_id = Some("oracle-run-missing-meta".to_string());
        app.oracle_state
            .inflight_runs
            .insert(thread_id, "oracle-run-missing-meta".to_string());
        app.oracle_state.pending_turn_id = Some("oracle-turn-1".to_string());
        app.oracle_state.inflight_run_requests.insert(
            "oracle-run-missing-meta".to_string(),
            OracleRunRequest {
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                session_slug: "oracle-session-chat".to_string(),
                prompt: "hello oracle".to_string(),
                requested_prompt: "hello oracle".to_string(),
                source_user_text: Some("hello oracle".to_string()),
                files: Vec::new(),
                workspace_cwd: app.chat_widget.config_ref().cwd.to_path_buf(),
                oracle_repo: app.chat_widget.config_ref().cwd.to_path_buf(),
                followup_session: Some("runtime-root".to_string()),
                model: OracleModelPreset::Thinking,
                browser_model_strategy: "select".to_string(),
                browser_model_label: Some("Thinking 5.5".to_string()),
                browser_thinking_time: None,
                requires_control: false,
                repair_attempt: 0,
                transport_retry_attempt: 0,
            },
        );
        app.persist_active_oracle_binding();
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-1".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-1".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-1".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "hello oracle".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }
        remember_oracle_run_remote_metadata(
            "oracle-run-missing-meta",
            None,
            Some("Attached Thread".to_string()),
        );

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-missing-meta".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-chat".to_string(),
                session_id: "oracle-session-chat-2".to_string(),
                requested_prompt: "hello oracle".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_oracle_response("Hello back."),
            },
        )
        .await?;

        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        let turn = store
            .turns
            .iter()
            .find(|turn| turn.id == "oracle-turn-1")
            .expect("oracle turn");
        assert_eq!(turn.status, TurnStatus::Completed);
        assert!(turn.items.iter().any(|item| matches!(
            item,
            ThreadItem::AgentMessage { text, .. } if text == "Hello back."
        )));
        assert!(!turn.items.iter().any(|item| matches!(
            item,
            ThreadItem::AgentMessage { text, .. }
                if text.contains("Oracle failed: Oracle thread identity violation")
        )));
        drop(store);
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(binding.conversation_id.as_deref(), Some("attached-1"));
        Ok(())
    }

    #[tokio::test]
    async fn late_aborted_missing_metadata_does_not_clear_newer_oracle_thread_session_state()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.set_oracle_remote_metadata(
            thread_id,
            Some("attached-1".to_string()),
            Some("Attached Thread".to_string()),
        );
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.active_run_id = Some("oracle-run-old".to_string());
        app.oracle_state
            .inflight_runs
            .insert(thread_id, "oracle-run-old".to_string());
        app.oracle_state.pending_turn_id = Some("oracle-turn-old".to_string());
        app.persist_active_oracle_binding();
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-old".to_string());
        }

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let handled = app
            .interrupt_oracle_thread(&mut app_server, thread_id)
            .await?;
        assert!(handled);

        app.set_oracle_thread_broker_session_id(thread_id, Some("runtime-new".to_string()));
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.active_run_id = Some("oracle-run-new".to_string());
        app.oracle_state
            .inflight_runs
            .insert(thread_id, "oracle-run-new".to_string());
        app.oracle_state.pending_turn_id = Some("oracle-turn-new".to_string());
        app.persist_active_oracle_binding();
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-new".to_string());
        }

        remember_oracle_run_remote_metadata(
            "oracle-run-old",
            None,
            Some("Attached Thread".to_string()),
        );

        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-old".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-old".to_string(),
                session_id: "oracle-session-old".to_string(),
                requested_prompt: "old oracle turn".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_oracle_response("stale"),
            },
        )
        .await?;

        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(binding.current_session_id.as_deref(), Some("runtime-new"));
        assert_eq!(binding.active_run_id.as_deref(), Some("oracle-run-new"));
        Ok(())
    }

    #[tokio::test]
    async fn late_aborted_oracle_completion_does_not_block_retry_completion_after_run_id_drift()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.active_run_id = Some("oracle-run-old".to_string());
        app.oracle_state
            .inflight_runs
            .insert(thread_id, "oracle-run-old".to_string());
        app.oracle_state.pending_turn_id = Some("oracle-turn-old".to_string());
        app.persist_active_oracle_binding();
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-old".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-old".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-old".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "old oracle turn".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let handled = app
            .interrupt_oracle_thread(&mut app_server, thread_id)
            .await?;
        assert!(handled);

        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.active_run_id = Some("oracle-run-new".to_string());
        app.oracle_state
            .inflight_runs
            .insert(thread_id, "oracle-run-new".to_string());
        app.oracle_state.pending_turn_id = Some("oracle-turn-new".to_string());
        app.persist_active_oracle_binding();
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-new".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-new".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-new".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "new oracle turn".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }

        // Simulate the stale binding drift seen after an interrupted run is retried.
        app.oracle_state.active_run_id = None;
        if let Some(binding) = app.oracle_state.bindings.get_mut(&thread_id) {
            binding.active_run_id = None;
        }

        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-old".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "old oracle turn".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_oracle_response("stale"),
            },
        )
        .await?;

        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn)
        );
        assert!(app.oracle_state.inflight_runs.contains_key(&thread_id));

        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-new".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a-2".to_string(),
                requested_prompt: "new oracle turn".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: parse_oracle_response("RESUME"),
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.active_run_id, None);
        assert!(!app.oracle_state.inflight_runs.contains_key(&thread_id));
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        let turn = store
            .turns
            .iter()
            .find(|turn| turn.id == "oracle-turn-new")
            .expect("oracle retry turn");
        assert_eq!(turn.status, TurnStatus::Completed);
        assert!(turn.items.iter().any(|item| matches!(
            item,
            ThreadItem::AgentMessage { text, .. } if text == "RESUME"
        )));
        Ok(())
    }

    #[tokio::test]
    async fn queued_orchestrator_checkpoint_is_dropped_when_workflow_version_advances() -> Result<()>
    {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 5,
            orchestrator_thread_id: Some(orchestrator_thread_id),
            ..Default::default()
        });
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.persist_active_oracle_binding();
        app.thread_event_channels.insert(
            orchestrator_thread_id,
            ThreadEventChannel::new(/*capacity*/ 4),
        );

        app.enqueue_thread_notification(
            orchestrator_thread_id,
            turn_completed_notification(
                orchestrator_thread_id,
                "orchestrator-turn-1",
                TurnStatus::Completed,
            ),
        )
        .await?;

        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert_eq!(
            binding.pending_checkpoint_threads,
            vec![orchestrator_thread_id]
        );
        assert_eq!(
            binding
                .pending_checkpoint_versions
                .get(&orchestrator_thread_id),
            Some(&5)
        );

        app.oracle_state.phase = OracleSupervisorPhase::Idle;
        if let Some(workflow) = app.oracle_state.workflow.as_mut() {
            workflow.version = 6;
        }
        app.persist_active_oracle_binding();

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.maybe_process_pending_oracle_checkpoints(&mut app_server, oracle_thread_id)
            .await?;

        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert!(binding.pending_checkpoint_threads.is_empty());
        assert!(binding.pending_checkpoint_versions.is_empty());
        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        Ok(())
    }

    #[tokio::test]
    async fn stale_checkpoint_advances_next_pending_checkpoint() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let stale_thread_id = ThreadId::new();
        let next_thread_id = ThreadId::new();
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 6,
            orchestrator_thread_id: Some(stale_thread_id),
            ..Default::default()
        });
        app.oracle_state.oracle_thread_id = Some(oracle_thread_id);
        app.oracle_state.orchestrator_thread_id = Some(stale_thread_id);
        app.oracle_state.phase = OracleSupervisorPhase::Idle;
        app.oracle_state.bindings.insert(
            oracle_thread_id,
            OracleThreadBinding {
                pending_checkpoint_threads: vec![stale_thread_id, next_thread_id],
                pending_checkpoint_versions: HashMap::from([
                    (stale_thread_id, 5),
                    (next_thread_id, 6),
                ]),
                ..Default::default()
            },
        );

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_orchestrator_checkpoint(&mut app_server, stale_thread_id, Some(5))
            .await?;

        assert_matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::OracleCheckpoint { thread_id, workflow_version })
                if thread_id == next_thread_id && workflow_version == Some(6)
        );
        Ok(())
    }

    #[tokio::test]
    async fn waiting_for_orchestrator_still_processes_queued_checkpoint() -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 6,
            orchestrator_thread_id: Some(orchestrator_thread_id),
            ..Default::default()
        });
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.oracle_state.phase = OracleSupervisorPhase::WaitingForOrchestrator;
        if let Some(binding) = app.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.pending_checkpoint_threads = vec![orchestrator_thread_id];
            binding
                .pending_checkpoint_versions
                .insert(orchestrator_thread_id, 5);
        }
        app.persist_active_oracle_binding();

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.maybe_process_pending_oracle_checkpoints(&mut app_server, oracle_thread_id)
            .await?;

        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert!(binding.pending_checkpoint_threads.is_empty());
        assert!(binding.pending_checkpoint_versions.is_empty());
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOrchestrator
        );
        Ok(())
    }

    #[tokio::test]
    async fn checkpoint_event_while_waiting_for_oracle_leaves_pending_checkpoint_queued()
    -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 6,
            orchestrator_thread_id: Some(orchestrator_thread_id),
            ..Default::default()
        });
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint);
        if let Some(binding) = app.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.pending_checkpoint_threads = vec![orchestrator_thread_id];
            binding
                .pending_checkpoint_versions
                .insert(orchestrator_thread_id, 6);
            binding
                .pending_checkpoint_turn_ids
                .insert(orchestrator_thread_id, "orchestrator-turn-1".to_string());
        }
        app.persist_active_oracle_binding();

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_orchestrator_checkpoint(&mut app_server, orchestrator_thread_id, Some(6))
            .await?;

        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert_eq!(
            binding.pending_checkpoint_threads,
            vec![orchestrator_thread_id]
        );
        assert_eq!(
            binding
                .pending_checkpoint_versions
                .get(&orchestrator_thread_id),
            Some(&6)
        );
        assert_eq!(
            binding
                .pending_checkpoint_turn_ids
                .get(&orchestrator_thread_id)
                .map(String::as_str),
            Some("orchestrator-turn-1")
        );
        Ok(())
    }

    #[tokio::test]
    async fn checkpoint_launch_failure_preserves_known_turn_and_schedules_retry() -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();

        app.requeue_orchestrator_checkpoint_launch_failure(
            oracle_thread_id,
            orchestrator_thread_id,
            Some(6),
            "orchestrator-turn-7",
        );

        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert_eq!(
            binding.pending_checkpoint_threads,
            vec![orchestrator_thread_id]
        );
        assert_eq!(
            binding
                .pending_checkpoint_versions
                .get(&orchestrator_thread_id),
            Some(&6)
        );
        assert_eq!(
            binding
                .pending_checkpoint_turn_ids
                .get(&orchestrator_thread_id)
                .map(String::as_str),
            Some("orchestrator-turn-7")
        );

        let (thread_id, workflow_version) = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match app_event_rx.recv().await {
                    Some(AppEvent::OracleCheckpoint {
                        thread_id,
                        workflow_version,
                    }) => return (thread_id, workflow_version),
                    Some(_) => continue,
                    None => panic!("app event channel closed"),
                }
            }
        })
        .await
        .expect("checkpoint retry event");
        assert_eq!(thread_id, orchestrator_thread_id);
        assert_eq!(workflow_version, Some(6));
        Ok(())
    }

    #[tokio::test]
    async fn deferred_oracle_checkpoint_moves_active_review_to_back_of_queue() {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let active_thread_id = ThreadId::new();
        let queued_thread_id = ThreadId::new();

        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            version: 5,
            ..Default::default()
        });
        if let Some(binding) = app.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.pending_checkpoint_threads = vec![queued_thread_id];
        }
        app.mark_active_oracle_checkpoint(
            oracle_thread_id,
            active_thread_id,
            "orchestrator-turn-1".to_string(),
        );

        let had_other_pending = app.defer_active_oracle_checkpoint(oracle_thread_id);

        assert!(had_other_pending);
        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert_eq!(binding.active_checkpoint_thread_id, None);
        assert_eq!(
            binding.pending_checkpoint_threads,
            vec![queued_thread_id, active_thread_id]
        );
        assert_eq!(
            binding.pending_checkpoint_versions.get(&active_thread_id),
            Some(&5)
        );
    }

    #[tokio::test]
    async fn deferred_oracle_checkpoint_preserves_newer_same_thread_pending_turn() {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let thread_id = ThreadId::new();

        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            version: 5,
            ..Default::default()
        });
        if let Some(binding) = app.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.pending_checkpoint_threads = vec![thread_id];
            binding.pending_checkpoint_versions.insert(thread_id, 5);
            binding
                .pending_checkpoint_turn_ids
                .insert(thread_id, "turn-2".to_string());
        }
        app.mark_active_oracle_checkpoint(oracle_thread_id, thread_id, "turn-1".to_string());

        let had_other_pending = app.defer_active_oracle_checkpoint(oracle_thread_id);

        assert!(had_other_pending);
        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert_eq!(binding.pending_checkpoint_threads, vec![thread_id]);
        assert_eq!(
            binding
                .pending_checkpoint_turn_ids
                .get(&thread_id)
                .map(String::as_str),
            Some("turn-2")
        );
    }

    #[tokio::test]
    async fn deferred_single_oracle_checkpoint_schedules_retry_event() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let active_thread_id = ThreadId::new();

        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            version: 5,
            ..Default::default()
        });
        app.mark_active_oracle_checkpoint(
            oracle_thread_id,
            active_thread_id,
            "orchestrator-turn-1".to_string(),
        );
        while app_event_rx.try_recv().is_ok() {}

        let had_other_pending = app.defer_active_oracle_checkpoint(oracle_thread_id);

        assert!(!had_other_pending);
        tokio::time::sleep(Duration::from_millis(250)).await;
        let retry = loop {
            match app_event_rx.try_recv() {
                Ok(AppEvent::OracleCheckpoint {
                    thread_id,
                    workflow_version,
                }) if thread_id == active_thread_id => break (thread_id, workflow_version),
                Ok(_) => continue,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    panic!("missing retry event")
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    panic!("app event channel closed unexpectedly")
                }
            }
        };
        assert_eq!(retry.0, active_thread_id);
        assert_eq!(retry.1, Some(5));
    }

    #[tokio::test]
    async fn malformed_oracle_delegate_completes_the_pending_turn() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.pending_turn_id = Some("oracle-turn-1".to_string());
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-1".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-1".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-1".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "ship the feature".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.oracle_state.active_run_id = Some("oracle-run-malformed".to_string());
        app.persist_active_oracle_binding();
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-malformed".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "ship the feature".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::Delegate,
                    message_for_user: String::new(),
                    task_for_orchestrator: None,
                    context_requests: Vec::new(),
                    directive: None,
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.pending_turn_id, None);
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert_eq!(store.active_turn_id, None);
        let turn = store
            .turns
            .iter()
            .find(|turn| turn.id == "oracle-turn-1")
            .expect("oracle turn");
        assert_eq!(turn.status, TurnStatus::Completed);
        let replay_turn = store
            .replay_entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                ThreadReplayEntry::Turn(turn) if turn.id == "oracle-turn-1" => Some(turn),
                _ => None,
            })
            .expect("oracle replay turn");
        assert_eq!(replay_turn.status, TurnStatus::Completed);
        assert_matches!(
            replay_turn.items.first(),
            Some(ThreadItem::UserMessage { .. })
        );
        assert!(!turn.items.iter().any(|item| matches!(
            item,
            ThreadItem::AgentMessage { text, .. }
                if text.contains("without an orchestrator task")
        )));
        assert!(store.buffer.iter().any(|event| matches!(
            event,
            ThreadBufferedEvent::OracleWorkflowEvent(OracleWorkflowThreadEvent { title, .. })
                if title.contains("without an orchestrator task")
        )));
        Ok(())
    }

    #[tokio::test]
    async fn oracle_handoff_without_message_uses_workflow_metadata_as_delegate_task() -> Result<()>
    {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.oracle_state.active_run_id = Some("oracle-run-handoff-metadata".to_string());
        app.persist_active_oracle_binding();

        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-handoff-metadata".to_string(),
                oracle_thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "User turn prompt".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: false,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::Delegate,
                    message_for_user: String::new(),
                    task_for_orchestrator: None,
                    context_requests: Vec::new(),
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-handoff-metadata".to_string()),
                        idempotency_key: Some("idem-handoff-metadata".to_string()),
                        op: Some(OracleControlOp::Handoff),
                        workflow_id: Some("oracle-routing".to_string()),
                        workflow_version: Some(3),
                        objective: Some(
                            "Verify the Codex harness loop end-to-end by delegating one trivial task to the orchestrator. Ask the orchestrator to compute 2+2 and return the result to Oracle."
                                .to_string(),
                        ),
                        summary: Some(
                            "User requested a full harness-loop verification via orchestrator delegation. Required task: orchestrator computes 2+2 and returns the result here; only then report back to the human."
                                .to_string(),
                        ),
                        status: Some("awaiting_orchestrator_result".to_string()),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        let orchestrator_thread_id = app
            .oracle_state
            .orchestrator_thread_id
            .expect("orchestrator thread");
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOrchestrator
        );
        assert_eq!(
            app.oracle_state.last_orchestrator_task.as_deref(),
            Some(
                "Verify the Codex harness loop end-to-end by delegating one trivial task to the orchestrator. Ask the orchestrator to compute 2+2 and return the result to Oracle.\n\nWorkflow summary: User requested a full harness-loop verification via orchestrator delegation. Required task: orchestrator computes 2+2 and returns the result here; only then report back to the human."
            )
        );
        let workflow = app.oracle_state.workflow.clone().expect("workflow binding");
        assert_eq!(workflow.workflow_id, "oracle-routing");
        assert_eq!(workflow.version, 3);
        assert_eq!(
            workflow.orchestrator_thread_id,
            Some(orchestrator_thread_id)
        );
        assert_eq!(
            workflow.objective.as_deref(),
            Some(
                "Verify the Codex harness loop end-to-end by delegating one trivial task to the orchestrator. Ask the orchestrator to compute 2+2 and return the result to Oracle."
            )
        );
        assert_eq!(
            workflow.summary.as_deref(),
            Some(
                "User requested a full harness-loop verification via orchestrator delegation. Required task: orchestrator computes 2+2 and returns the result here; only then report back to the human."
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn oracle_checkpoint_action_keeps_workflow_running() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 2,
            ..Default::default()
        });
        app.oracle_state.active_run_id = Some("oracle-run-checkpoint".to_string());
        app.persist_active_oracle_binding();
        app.begin_oracle_user_turn(
            thread_id,
            &[UserInput::Text {
                text: "Continue the workflow.".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .await;

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-checkpoint".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "Continue the workflow.".to_string(),
                source_user_text: Some("Continue the workflow.".to_string()),
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::Checkpoint,
                    message_for_user: "Milestone A complete; continuing.".to_string(),
                    task_for_orchestrator: None,
                    context_requests: Vec::new(),
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-checkpoint".to_string()),
                        idempotency_key: Some("idem-checkpoint".to_string()),
                        op: Some(OracleControlOp::Checkpoint),
                        workflow_id: Some("oracle-routing".to_string()),
                        workflow_version: Some(2),
                        summary: Some("Milestone A complete".to_string()),
                        status: Some("running".to_string()),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        let workflow = app.oracle_state.workflow.as_ref().expect("workflow");
        assert_eq!(workflow.status, OracleWorkflowStatus::Running);
        assert_eq!(workflow.summary.as_deref(), Some("Milestone A complete"));
        assert_eq!(
            workflow.last_checkpoint.as_deref(),
            Some("Milestone A complete")
        );
        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        Ok(())
    }

    #[tokio::test]
    async fn oracle_checkpoint_status_complete_is_clamped_to_running() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 2,
            ..Default::default()
        });
        app.oracle_state.active_run_id = Some("oracle-run-checkpoint-complete".to_string());
        app.persist_active_oracle_binding();
        app.begin_oracle_user_turn(
            thread_id,
            &[UserInput::Text {
                text: "Continue the workflow.".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .await;

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-checkpoint-complete".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "Continue the workflow.".to_string(),
                source_user_text: Some("Continue the workflow.".to_string()),
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::Checkpoint,
                    message_for_user: "Milestone A complete; continuing.".to_string(),
                    task_for_orchestrator: None,
                    context_requests: Vec::new(),
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-checkpoint-complete".to_string()),
                        idempotency_key: Some("idem-checkpoint-complete".to_string()),
                        op: Some(OracleControlOp::Checkpoint),
                        workflow_id: Some("oracle-routing".to_string()),
                        workflow_version: Some(2),
                        summary: Some("Milestone A complete".to_string()),
                        status: Some("complete".to_string()),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        let workflow = app.oracle_state.workflow.as_ref().expect("workflow");
        assert_eq!(workflow.status, OracleWorkflowStatus::Running);
        Ok(())
    }

    #[tokio::test]
    async fn oracle_reply_status_failed_is_clamped_to_running() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 2,
            ..Default::default()
        });
        app.oracle_state.active_run_id = Some("oracle-run-reply-failed".to_string());
        app.persist_active_oracle_binding();
        app.begin_oracle_user_turn(
            thread_id,
            &[UserInput::Text {
                text: "Continue the workflow.".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .await;

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-reply-failed".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "Continue the workflow.".to_string(),
                source_user_text: Some("Continue the workflow.".to_string()),
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::Reply,
                    message_for_user: "Need a follow-up decision.".to_string(),
                    task_for_orchestrator: None,
                    context_requests: Vec::new(),
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-reply-failed".to_string()),
                        idempotency_key: Some("idem-reply-failed".to_string()),
                        op: Some(OracleControlOp::Reply),
                        workflow_id: Some("oracle-routing".to_string()),
                        workflow_version: Some(2),
                        summary: Some("Waiting on a decision".to_string()),
                        status: Some("failed".to_string()),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        let workflow = app.oracle_state.workflow.as_ref().expect("workflow");
        assert_eq!(workflow.status, OracleWorkflowStatus::NeedsHuman);
        assert_ne!(workflow.status, OracleWorkflowStatus::Failed);
        Ok(())
    }

    #[tokio::test]
    async fn terminal_workflow_drops_pending_orchestrator_checkpoints() -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Complete,
            version: 2,
            orchestrator_thread_id: Some(orchestrator_thread_id),
            ..Default::default()
        });
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        if let Some(binding) = app.oracle_state.bindings.get_mut(&oracle_thread_id) {
            binding.orchestrator_thread_id = Some(orchestrator_thread_id);
            binding.pending_checkpoint_threads = vec![orchestrator_thread_id];
            binding
                .pending_checkpoint_versions
                .insert(orchestrator_thread_id, 2);
        }

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.maybe_process_pending_oracle_checkpoints(&mut app_server, oracle_thread_id)
            .await?;

        let binding = app
            .oracle_state
            .bindings
            .get(&oracle_thread_id)
            .expect("oracle binding");
        assert!(binding.pending_checkpoint_threads.is_empty());
        assert!(binding.pending_checkpoint_versions.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn oracle_finish_status_running_is_clamped_to_complete() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let old_orchestrator_thread_id = ThreadId::new();
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 2,
            orchestrator_thread_id: Some(old_orchestrator_thread_id),
            ..Default::default()
        });
        app.oracle_state.orchestrator_thread_id = Some(old_orchestrator_thread_id);
        app.oracle_state.active_checkpoint_thread_id = Some(old_orchestrator_thread_id);
        app.oracle_state.active_checkpoint_turn_id = Some("orchestrator-turn-active".to_string());
        if let Some(binding) = app.oracle_state.bindings.get_mut(&thread_id) {
            binding.orchestrator_thread_id = Some(old_orchestrator_thread_id);
            binding.pending_checkpoint_threads = vec![old_orchestrator_thread_id];
            binding
                .pending_checkpoint_versions
                .insert(old_orchestrator_thread_id, 2);
            binding.pending_checkpoint_turn_ids.insert(
                old_orchestrator_thread_id,
                "orchestrator-turn-pending".to_string(),
            );
            binding.last_checkpoint_turn_ids.insert(
                old_orchestrator_thread_id,
                "orchestrator-turn-old".to_string(),
            );
            binding.active_checkpoint_thread_id = Some(old_orchestrator_thread_id);
            binding.active_checkpoint_turn_id = Some("orchestrator-turn-active".to_string());
        }
        app.oracle_state.active_run_id = Some("oracle-run-finish-running".to_string());
        app.persist_active_oracle_binding();
        app.begin_oracle_user_turn(
            thread_id,
            &[UserInput::Text {
                text: "Finish the workflow.".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .await;

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-finish-running".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "Finish the workflow.".to_string(),
                source_user_text: Some("Finish the workflow.".to_string()),
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::Finish,
                    message_for_user: "All done.".to_string(),
                    task_for_orchestrator: None,
                    context_requests: Vec::new(),
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-finish-running".to_string()),
                        idempotency_key: Some("idem-finish-running".to_string()),
                        op: Some(OracleControlOp::Finish),
                        workflow_id: Some("oracle-routing".to_string()),
                        workflow_version: Some(2),
                        summary: Some("Workflow complete".to_string()),
                        status: Some("running".to_string()),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        let workflow = app.oracle_state.workflow.as_ref().expect("workflow");
        assert_eq!(workflow.status, OracleWorkflowStatus::Complete);
        assert_eq!(workflow.orchestrator_thread_id, None);
        assert_eq!(app.oracle_state.orchestrator_thread_id, None);
        assert_eq!(app.oracle_state.active_checkpoint_thread_id, None);
        assert_eq!(app.oracle_state.active_checkpoint_turn_id, None);
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(binding.orchestrator_thread_id, None);
        assert!(binding.pending_checkpoint_threads.is_empty());
        assert!(binding.pending_checkpoint_versions.is_empty());
        assert!(binding.pending_checkpoint_turn_ids.is_empty());
        assert!(binding.last_checkpoint_turn_ids.is_empty());
        assert_eq!(binding.active_checkpoint_thread_id, None);
        assert_eq!(binding.active_checkpoint_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn oracle_finish_status_failed_marks_workflow_failed_and_clears_orchestrator_state()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let old_orchestrator_thread_id = ThreadId::new();
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 2,
            orchestrator_thread_id: Some(old_orchestrator_thread_id),
            ..Default::default()
        });
        app.oracle_state.orchestrator_thread_id = Some(old_orchestrator_thread_id);
        app.oracle_state.active_checkpoint_thread_id = Some(old_orchestrator_thread_id);
        app.oracle_state.active_checkpoint_turn_id = Some("orchestrator-turn-active".to_string());
        if let Some(binding) = app.oracle_state.bindings.get_mut(&thread_id) {
            binding.orchestrator_thread_id = Some(old_orchestrator_thread_id);
            binding.pending_checkpoint_threads = vec![old_orchestrator_thread_id];
            binding
                .pending_checkpoint_versions
                .insert(old_orchestrator_thread_id, 2);
            binding.pending_checkpoint_turn_ids.insert(
                old_orchestrator_thread_id,
                "orchestrator-turn-pending".to_string(),
            );
            binding.last_checkpoint_turn_ids.insert(
                old_orchestrator_thread_id,
                "orchestrator-turn-old".to_string(),
            );
            binding.active_checkpoint_thread_id = Some(old_orchestrator_thread_id);
            binding.active_checkpoint_turn_id = Some("orchestrator-turn-active".to_string());
        }
        app.oracle_state.active_run_id = Some("oracle-run-finish-failed".to_string());
        app.persist_active_oracle_binding();
        app.begin_oracle_user_turn(
            thread_id,
            &[UserInput::Text {
                text: "Fail the workflow.".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .await;

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-finish-failed".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-failed".to_string(),
                session_id: "oracle-session-failed".to_string(),
                requested_prompt: "Fail the workflow.".to_string(),
                source_user_text: Some("Fail the workflow.".to_string()),
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::Finish,
                    message_for_user: String::new(),
                    task_for_orchestrator: None,
                    context_requests: Vec::new(),
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-finish-failed".to_string()),
                        idempotency_key: Some("idem-finish-failed".to_string()),
                        op: Some(OracleControlOp::Finish),
                        workflow_id: Some("oracle-routing".to_string()),
                        workflow_version: Some(2),
                        summary: Some("Workflow failed".to_string()),
                        status: Some("failed".to_string()),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        let workflow = app.oracle_state.workflow.as_ref().expect("workflow");
        assert_eq!(workflow.status, OracleWorkflowStatus::Failed);
        assert_eq!(workflow.summary.as_deref(), Some("Workflow failed"));
        assert_eq!(workflow.orchestrator_thread_id, None);
        assert_eq!(
            app.oracle_state.last_status.as_deref(),
            Some("Oracle marked the workflow failed.")
        );
        assert_eq!(app.oracle_state.orchestrator_thread_id, None);
        assert_eq!(app.oracle_state.active_checkpoint_thread_id, None);
        assert_eq!(app.oracle_state.active_checkpoint_turn_id, None);
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(binding.orchestrator_thread_id, None);
        assert!(binding.pending_checkpoint_threads.is_empty());
        assert!(binding.pending_checkpoint_versions.is_empty());
        assert!(binding.pending_checkpoint_turn_ids.is_empty());
        assert!(binding.last_checkpoint_turn_ids.is_empty());
        assert_eq!(binding.active_checkpoint_thread_id, None);
        assert_eq!(binding.active_checkpoint_turn_id, None);
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert!(store.turns.iter().any(|turn| {
            turn.items.iter().any(|item| matches!(
            item,
            ThreadItem::AgentMessage { text, .. } if text == "Oracle marked the workflow failed."
        ))
        }));
        Ok(())
    }

    #[tokio::test]
    async fn replacement_workflow_without_handoff_clears_stale_orchestrator_thread() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let old_orchestrator_thread_id = ThreadId::new();
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing-old".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Complete,
            version: 4,
            orchestrator_thread_id: Some(old_orchestrator_thread_id),
            ..Default::default()
        });
        app.oracle_state.orchestrator_thread_id = Some(old_orchestrator_thread_id);
        app.oracle_state.active_checkpoint_thread_id = Some(old_orchestrator_thread_id);
        app.oracle_state.active_checkpoint_turn_id = Some("orchestrator-turn-active".to_string());
        if let Some(binding) = app.oracle_state.bindings.get_mut(&thread_id) {
            binding.pending_checkpoint_threads = vec![old_orchestrator_thread_id];
            binding
                .pending_checkpoint_versions
                .insert(old_orchestrator_thread_id, 4);
            binding.pending_checkpoint_turn_ids.insert(
                old_orchestrator_thread_id,
                "orchestrator-turn-pending".to_string(),
            );
            binding.last_checkpoint_turn_ids.insert(
                old_orchestrator_thread_id,
                "orchestrator-turn-old".to_string(),
            );
            binding.active_checkpoint_thread_id = Some(old_orchestrator_thread_id);
            binding.active_checkpoint_turn_id = Some("orchestrator-turn-active".to_string());
        }
        app.reopen_active_oracle_workflow_for_user_turn();
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.pending_turn_id = Some("oracle-turn-replacement-ask-user".to_string());
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-replacement-ask-user".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-replacement-ask-user".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-replacement-ask-user".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "Start a new workflow but ask me something first.".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }
        app.oracle_state.active_run_id = Some("oracle-run-replacement-ask-user".to_string());
        app.persist_active_oracle_binding();

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-replacement-ask-user".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "Start a new workflow but ask me something first.".to_string(),
                source_user_text: Some(
                    "Start a new workflow but ask me something first.".to_string(),
                ),
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::AskUser,
                    message_for_user: "Which environment should I use?".to_string(),
                    task_for_orchestrator: None,
                    context_requests: Vec::new(),
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-replacement-ask-user".to_string()),
                        idempotency_key: Some("idem-replacement-ask-user".to_string()),
                        workflow_id: Some("oracle-routing-new".to_string()),
                        workflow_version: Some(1),
                        summary: Some("Waiting on environment choice".to_string()),
                        status: Some("needs_human".to_string()),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        let workflow = app
            .oracle_state
            .workflow
            .as_ref()
            .expect("replacement workflow");
        assert_eq!(workflow.workflow_id, "oracle-routing-new");
        assert_eq!(workflow.orchestrator_thread_id, None);
        assert_eq!(app.oracle_state.orchestrator_thread_id, None);
        assert_eq!(app.oracle_state.active_checkpoint_thread_id, None);
        assert_eq!(app.oracle_state.active_checkpoint_turn_id, None);
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert!(binding.pending_checkpoint_threads.is_empty());
        assert!(binding.pending_checkpoint_versions.is_empty());
        assert!(binding.pending_checkpoint_turn_ids.is_empty());
        assert!(binding.last_checkpoint_turn_ids.is_empty());
        assert_eq!(binding.active_checkpoint_thread_id, None);
        assert_eq!(binding.active_checkpoint_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn direct_retry_preserves_completed_workflow_replacement_semantics() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let oracle_repo =
            find_oracle_repo(app.chat_widget.config_ref().cwd.as_path()).expect("oracle repo");
        app.oracle_broker = Some((oracle_repo.clone(), OracleBrokerClient::new_test_client()));
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing-old".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Complete,
            version: 4,
            ..Default::default()
        });
        app.reopen_active_oracle_workflow_for_user_turn();
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.active_run_id = Some("oracle-run-old".to_string());
        app.oracle_state
            .inflight_runs
            .insert(thread_id, "oracle-run-old".to_string());
        app.oracle_state.inflight_run_requests.insert(
            "oracle-run-old".to_string(),
            OracleRunRequest {
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                session_slug: "oracle-session-root".to_string(),
                prompt: "Start a new workflow but ask me something first.".to_string(),
                requested_prompt: "Start a new workflow but ask me something first.".to_string(),
                source_user_text: Some(
                    "Start a new workflow but ask me something first.".to_string(),
                ),
                files: Vec::new(),
                workspace_cwd: app.chat_widget.config_ref().cwd.to_path_buf(),
                oracle_repo,
                followup_session: None,
                model: OracleModelPreset::Thinking,
                browser_model_strategy: "select".to_string(),
                browser_model_label: Some("Thinking 5.5".to_string()),
                browser_thinking_time: None,
                requires_control: false,
                repair_attempt: 0,
                transport_retry_attempt: 0,
            },
        );

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_failed(
            &mut app_server,
            "oracle-run-old".to_string(),
            thread_id,
            OracleRequestKind::UserTurn,
            "codex-oracle-old".to_string(),
            "Oracle broker closed before replying".to_string(),
        )
        .await?;

        assert!(app.oracle_state.accept_user_turn_workflow_replacement);
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn)
        );
        assert_eq!(app.oracle_state.inflight_run_requests.len(), 1);
        let retried = app
            .oracle_state
            .inflight_run_requests
            .values()
            .next()
            .expect("retried request");
        assert_eq!(retried.transport_retry_attempt, 1);
        assert!(!retried.requires_control);
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_oracle_handoff_idempotency_key_is_ignored() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 3,
            orchestrator_thread_id: Some(orchestrator_thread_id),
            applied_idempotency_keys: HashSet::from(["idem-dup".to_string()]),
            ..Default::default()
        });
        app.oracle_state.active_run_id = Some("oracle-run-dup".to_string());
        app.persist_active_oracle_binding();
        app.begin_oracle_user_turn(
            thread_id,
            &[UserInput::Text {
                text: "Duplicate handoff".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .await;

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-dup".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "Duplicate handoff".to_string(),
                source_user_text: Some("Duplicate handoff".to_string()),
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::Delegate,
                    message_for_user: String::new(),
                    task_for_orchestrator: Some("Do work".to_string()),
                    context_requests: Vec::new(),
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-dup".to_string()),
                        idempotency_key: Some("idem-dup".to_string()),
                        op: Some(OracleControlOp::Handoff),
                        workflow_id: Some("oracle-routing".to_string()),
                        workflow_version: Some(3),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOrchestrator
        );
        assert_eq!(
            app.oracle_state.orchestrator_thread_id,
            Some(orchestrator_thread_id)
        );
        let workflow = app.oracle_state.workflow.as_ref().expect("workflow");
        assert_eq!(workflow.applied_idempotency_keys.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_inflight_oracle_handoff_idempotency_key_is_ignored() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 3,
            orchestrator_thread_id: Some(orchestrator_thread_id),
            pending_idempotency_keys: HashSet::from(["idem-inflight".to_string()]),
            ..Default::default()
        });
        app.oracle_state.active_run_id = Some("oracle-run-inflight-dup".to_string());
        app.persist_active_oracle_binding();
        app.begin_oracle_user_turn(
            thread_id,
            &[UserInput::Text {
                text: "Duplicate inflight handoff".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .await;

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-inflight-dup".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-inflight".to_string(),
                session_id: "oracle-session-inflight".to_string(),
                requested_prompt: "Duplicate inflight handoff".to_string(),
                source_user_text: Some("Duplicate inflight handoff".to_string()),
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::Delegate,
                    message_for_user: String::new(),
                    task_for_orchestrator: Some("Do work".to_string()),
                    context_requests: Vec::new(),
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-inflight".to_string()),
                        idempotency_key: Some("idem-inflight".to_string()),
                        op: Some(OracleControlOp::Handoff),
                        workflow_id: Some("oracle-routing".to_string()),
                        workflow_version: Some(3),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOrchestrator
        );
        let workflow = app.oracle_state.workflow.as_ref().expect("workflow");
        assert_eq!(workflow.pending_idempotency_keys.len(), 1);
        assert_eq!(workflow.applied_idempotency_keys.len(), 0);
        assert!(
            app.oracle_state
                .last_status
                .as_deref()
                .is_some_and(|message| message.contains("Ignored duplicate Oracle control retry"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn oracle_handoff_submit_failure_does_not_crash_tui() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let bogus_orchestrator_thread_id = ThreadId::new();
        app.oracle_state.orchestrator_thread_id = Some(bogus_orchestrator_thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.active_run_id = Some("oracle-run-handoff-fail".to_string());
        app.persist_active_oracle_binding();
        app.begin_oracle_user_turn(
            thread_id,
            &[UserInput::Text {
                text: "Please do the work".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .await;
        app.ensure_thread_channel(bogus_orchestrator_thread_id);

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-handoff-fail".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-handoff".to_string(),
                session_id: "oracle-session-handoff".to_string(),
                requested_prompt: "Please do the work".to_string(),
                source_user_text: Some("Please do the work".to_string()),
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::Delegate,
                    message_for_user: "Starting orchestrator work.".to_string(),
                    task_for_orchestrator: Some("Do the work".to_string()),
                    context_requests: Vec::new(),
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-handoff-fail".to_string()),
                        idempotency_key: Some("idem-handoff-fail".to_string()),
                        op: Some(OracleControlOp::Handoff),
                        workflow_id: Some("oracle-routing".to_string()),
                        workflow_version: Some(1),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert!(
            app.oracle_state
                .last_status
                .as_deref()
                .is_some_and(|message| message.contains("failed to start the orchestrator turn"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn reopened_workflow_clears_duplicate_control_history_for_new_version() -> Result<()> {
        let mut app = make_test_app().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::NeedsHuman,
            version: 3,
            pending_op_ids: HashSet::from(["op-pending".to_string()]),
            pending_idempotency_keys: HashSet::from(["idem-pending".to_string()]),
            applied_op_ids: HashSet::from(["op-repeat".to_string()]),
            applied_idempotency_keys: HashSet::from(["idem-repeat".to_string()]),
            ..Default::default()
        });

        app.reopen_active_oracle_workflow_for_user_turn();

        let workflow = app.oracle_state.workflow.as_ref().expect("workflow");
        assert_eq!(workflow.status, OracleWorkflowStatus::Running);
        assert_eq!(workflow.version, 4);
        assert!(workflow.pending_op_ids.is_empty());
        assert!(workflow.pending_idempotency_keys.is_empty());
        assert!(workflow.applied_op_ids.is_empty());
        assert!(workflow.applied_idempotency_keys.is_empty());
        assert_eq!(
            app.oracle_duplicate_control_message(&OracleControlDirective {
                op_id: Some("op-repeat".to_string()),
                idempotency_key: Some("idem-repeat".to_string()),
                workflow_id: Some("oracle-routing".to_string()),
                workflow_version: Some(4),
                ..Default::default()
            }),
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_oracle_checkpoint_commits_the_active_checkpoint_turn() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint);
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 3,
            orchestrator_thread_id: Some(orchestrator_thread_id),
            applied_idempotency_keys: HashSet::from(["idem-checkpoint-dup".to_string()]),
            ..Default::default()
        });
        app.oracle_state.active_run_id = Some("oracle-run-checkpoint-dup".to_string());
        app.oracle_state.active_checkpoint_thread_id = Some(orchestrator_thread_id);
        app.oracle_state.active_checkpoint_turn_id = Some("orchestrator-turn-1".to_string());
        if let Some(binding) = app.oracle_state.bindings.get_mut(&thread_id) {
            binding.active_checkpoint_thread_id = Some(orchestrator_thread_id);
            binding.active_checkpoint_turn_id = Some("orchestrator-turn-1".to_string());
        }
        app.persist_active_oracle_binding();

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-checkpoint-dup".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::Checkpoint,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "Checkpoint prompt".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::Checkpoint,
                    message_for_user: "Duplicate checkpoint".to_string(),
                    task_for_orchestrator: None,
                    context_requests: Vec::new(),
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-checkpoint-dup".to_string()),
                        idempotency_key: Some("idem-checkpoint-dup".to_string()),
                        op: Some(OracleControlOp::Checkpoint),
                        workflow_id: Some("oracle-routing".to_string()),
                        workflow_version: Some(3),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOrchestrator
        );
        assert_eq!(app.oracle_state.active_checkpoint_thread_id, None);
        assert_eq!(app.oracle_state.active_checkpoint_turn_id, None);
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(binding.active_checkpoint_thread_id, None);
        assert_eq!(binding.active_checkpoint_turn_id, None);
        assert_eq!(
            binding
                .last_checkpoint_turn_ids
                .get(&orchestrator_thread_id)
                .map(String::as_str),
            Some("orchestrator-turn-1")
        );
        Ok(())
    }

    #[tokio::test]
    async fn exhausted_checkpoint_repair_drops_the_active_checkpoint_instead_of_requeueing()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint);
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 3,
            orchestrator_thread_id: Some(orchestrator_thread_id),
            ..Default::default()
        });
        app.oracle_state.active_run_id = Some("oracle-run-checkpoint-repair".to_string());
        app.mark_active_oracle_checkpoint(
            thread_id,
            orchestrator_thread_id,
            "orchestrator-turn-1".to_string(),
        );
        while app_event_rx.try_recv().is_ok() {}

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-checkpoint-repair".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::Checkpoint,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "Checkpoint prompt".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 1,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::Checkpoint,
                    message_for_user: String::new(),
                    task_for_orchestrator: None,
                    context_requests: Vec::new(),
                    directive: None,
                    control_issue: Some(crate::oracle_supervisor::OracleControlIssue {
                        kind: crate::oracle_supervisor::OracleControlIssueKind::InvalidSchema,
                        message: "Oracle control block was still invalid after repair.".to_string(),
                    }),
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.active_checkpoint_thread_id, None);
        assert_eq!(app.oracle_state.active_checkpoint_turn_id, None);
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert!(binding.pending_checkpoint_threads.is_empty());
        assert_eq!(
            binding
                .last_checkpoint_turn_ids
                .get(&orchestrator_thread_id)
                .map(String::as_str),
            Some("orchestrator-turn-1")
        );

        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(
            std::iter::from_fn(|| app_event_rx.try_recv().ok()).all(|event| {
                !matches!(
                    event,
                    AppEvent::OracleCheckpoint { thread_id, .. }
                        if thread_id == orchestrator_thread_id
                )
            }),
            "exhausted checkpoint repair should not reschedule the same checkpoint",
        );
        Ok(())
    }

    #[tokio::test]
    async fn oracle_request_context_read_failure_does_not_crash_tui() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let bogus_orchestrator_thread_id = ThreadId::new();
        app.oracle_state.orchestrator_thread_id = Some(bogus_orchestrator_thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.active_run_id = Some("oracle-run-context-fail".to_string());
        app.persist_active_oracle_binding();
        app.begin_oracle_user_turn(
            thread_id,
            &[UserInput::Text {
                text: "Need more context".to_string(),
                text_elements: Vec::new(),
            }],
        )
        .await;

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-context-fail".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                requested_slug: "oracle-session-context".to_string(),
                session_id: "oracle-session-context".to_string(),
                requested_prompt: "Need more context".to_string(),
                source_user_text: Some("Need more context".to_string()),
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::RequestContext,
                    message_for_user: "Need the latest diff.".to_string(),
                    task_for_orchestrator: None,
                    context_requests: vec!["git_diff".to_string()],
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-context-fail".to_string()),
                        idempotency_key: Some("idem-context-fail".to_string()),
                        op: Some(OracleControlOp::RequestContext),
                        workflow_id: Some("oracle-routing".to_string()),
                        workflow_version: Some(1),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert!(
            app.oracle_state
                .last_status
                .as_deref()
                .is_some_and(|message| message.contains("could not read orchestrator thread"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn checkpoint_request_context_without_requests_commits_the_active_checkpoint()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint);
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 3,
            orchestrator_thread_id: Some(orchestrator_thread_id),
            ..Default::default()
        });
        app.oracle_state.active_run_id = Some("oracle-run-checkpoint-context-empty".to_string());
        app.mark_active_oracle_checkpoint(
            thread_id,
            orchestrator_thread_id,
            "orchestrator-turn-1".to_string(),
        );

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-checkpoint-context-empty".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::Checkpoint,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "Checkpoint prompt".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::RequestContext,
                    message_for_user: String::new(),
                    task_for_orchestrator: None,
                    context_requests: Vec::new(),
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-checkpoint-context-empty".to_string()),
                        idempotency_key: Some("idem-checkpoint-context-empty".to_string()),
                        op: Some(OracleControlOp::RequestContext),
                        workflow_id: Some("oracle-routing".to_string()),
                        workflow_version: Some(3),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.active_checkpoint_thread_id, None);
        assert_eq!(app.oracle_state.active_checkpoint_turn_id, None);
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(
            binding
                .last_checkpoint_turn_ids
                .get(&orchestrator_thread_id)
                .map(String::as_str),
            Some("orchestrator-turn-1")
        );
        Ok(())
    }

    #[tokio::test]
    async fn repeated_checkpoint_request_context_commits_the_active_checkpoint() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint);
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 3,
            orchestrator_thread_id: Some(orchestrator_thread_id),
            ..Default::default()
        });
        app.oracle_state.automatic_context_followups = 1;
        app.oracle_state.active_run_id = Some("oracle-run-checkpoint-context-repeat".to_string());
        app.mark_active_oracle_checkpoint(
            thread_id,
            orchestrator_thread_id,
            "orchestrator-turn-1".to_string(),
        );

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.handle_oracle_run_completed(
            &mut app_server,
            OracleRunResult {
                run_id: "oracle-run-checkpoint-context-repeat".to_string(),
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::Checkpoint,
                requested_slug: "oracle-session-a".to_string(),
                session_id: "oracle-session-a".to_string(),
                requested_prompt: "Checkpoint prompt".to_string(),
                source_user_text: None,
                files: Vec::new(),
                requires_control: true,
                repair_attempt: 0,
                response: crate::oracle_supervisor::OracleResponse {
                    action: OracleAction::RequestContext,
                    message_for_user: String::new(),
                    task_for_orchestrator: None,
                    context_requests: vec!["file:src/lib.rs".to_string()],
                    directive: Some(OracleControlDirective {
                        schema_version: Some(ORACLE_CONTROL_SCHEMA_VERSION),
                        op_id: Some("op-checkpoint-context-repeat".to_string()),
                        idempotency_key: Some("idem-checkpoint-context-repeat".to_string()),
                        op: Some(OracleControlOp::RequestContext),
                        workflow_id: Some("oracle-routing".to_string()),
                        workflow_version: Some(3),
                        ..Default::default()
                    }),
                    control_issue: None,
                    raw_output: String::new(),
                },
            },
        )
        .await?;

        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.active_checkpoint_thread_id, None);
        assert_eq!(app.oracle_state.active_checkpoint_turn_id, None);
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(
            binding
                .last_checkpoint_turn_ids
                .get(&orchestrator_thread_id)
                .map(String::as_str),
            Some("orchestrator-turn-1")
        );
        Ok(())
    }

    #[tokio::test]
    async fn natural_language_oracle_entrypoint_routes_through_oracle_supervisor() -> Result<()> {
        let mut app = make_test_app().await;
        std::fs::create_dir_all(app.chat_widget.config_ref().cwd.as_path())?;
        let oracle_repo =
            find_oracle_repo(app.chat_widget.config_ref().cwd.as_path()).expect("oracle repo");
        app.oracle_broker = Some((oracle_repo, OracleBrokerClient::new_test_client()));
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let origin_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000111").expect("valid thread");

        let handled = app
            .maybe_route_natural_language_oracle_invocation(
                None,
                &mut app_server,
                origin_thread_id,
                &[UserInput::Text {
                    text: "Use your oracle skill to tell the orchestrator to compute 2+2 and report back.".to_string(),
                    text_elements: Vec::new(),
                }],
            )
            .await?;

        assert!(handled);

        let oracle_thread_id = app.oracle_state.oracle_thread_id.expect("oracle thread");
        assert_ne!(oracle_thread_id, origin_thread_id);
        assert!(app.oracle_state.bindings.contains_key(&oracle_thread_id));
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn)
        );
        assert!(app.oracle_state.active_run_id.is_some());
        app.shutdown_oracle_broker(true);
        Ok(())
    }

    #[test]
    fn natural_language_oracle_entrypoint_requires_explicit_leading_invocation() {
        let explicit = [UserInput::Text {
            text: "Use your oracle skill to tell the orchestrator to compute 2+2.".to_string(),
            text_elements: Vec::new(),
        }];
        assert!(App::natural_language_oracle_invocation(&explicit));

        let polite = [UserInput::Text {
            text: "Please use your oracle skill to review the protocol changes.".to_string(),
            text_elements: Vec::new(),
        }];
        assert!(App::natural_language_oracle_invocation(&polite));

        let quoted = [UserInput::Text {
            text: "The docs should literally say `use your oracle skill to do XYZ`.".to_string(),
            text_elements: Vec::new(),
        }];
        assert!(!App::natural_language_oracle_invocation(&quoted));

        let example = [UserInput::Text {
            text: "Example prompt:\nuse your oracle skill to do XYZ".to_string(),
            text_elements: Vec::new(),
        }];
        assert!(!App::natural_language_oracle_invocation(&example));
    }

    #[tokio::test]
    async fn natural_language_oracle_entrypoint_routes_into_the_visible_oracle_thread() -> Result<()>
    {
        let mut app = make_test_app().await;
        std::fs::create_dir_all(app.chat_widget.config_ref().cwd.as_path())?;
        let oracle_repo =
            find_oracle_repo(app.chat_widget.config_ref().cwd.as_path()).expect("oracle repo");
        app.oracle_broker = Some((oracle_repo, OracleBrokerClient::new_test_client()));
        let oracle_thread_id = app.ensure_visible_oracle_thread().await;
        let origin_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000111").expect("valid thread");
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");

        let handled = app
            .maybe_route_natural_language_oracle_invocation(
                None,
                &mut app_server,
                origin_thread_id,
                &[UserInput::Text {
                    text: "Use your oracle skill to review the protocol changes.".to_string(),
                    text_elements: Vec::new(),
                }],
            )
            .await?;

        assert!(handled);
        assert_eq!(app.oracle_state.oracle_thread_id, Some(oracle_thread_id));
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn)
        );
        assert!(app.oracle_state.pending_turn_id.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn oracle_user_turn_rewrites_requested_prompt_but_preserves_original_source_text()
    -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_repo =
            find_oracle_repo(app.chat_widget.config_ref().cwd.as_path()).expect("oracle repo");
        app.oracle_broker = Some((oracle_repo, OracleBrokerClient::new_test_client()));
        let thread_id = app.ensure_visible_oracle_thread().await;
        let workspace = app.chat_widget.config_ref().cwd.to_path_buf();
        let src = workspace.join("src");
        std::fs::create_dir_all(&src)?;
        std::fs::write(src.join("a.rs"), "fn a() {}\n")?;
        std::fs::write(src.join("b.rs"), "fn b() {}\n")?;

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let handled = app
            .handle_oracle_user_turn(
                &mut app_server,
                thread_id,
                &[
                    UserInput::Text {
                        text: "Review these files\nfile: src/a.rs\nglob: src/**/*.rs".to_string(),
                        text_elements: Vec::new(),
                    },
                    UserInput::LocalImage {
                        path: PathBuf::from("src/a.rs"),
                    },
                ],
            )
            .await?;

        assert!(handled);
        let request = app
            .oracle_state
            .inflight_run_requests
            .values()
            .next()
            .expect("oracle request");
        let actual_files = request
            .files
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let expected_files = [
            std::fs::canonicalize(src.join("a.rs"))?
                .display()
                .to_string(),
            std::fs::canonicalize(src.join("b.rs"))?
                .display()
                .to_string(),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual_files, expected_files);
        let source = request
            .source_user_text
            .as_deref()
            .expect("source user text");
        assert!(source.contains("file: src/a.rs"));
        assert!(source.contains("glob: src/**/*.rs"));
        assert!(!source.contains("[local_image: src/a.rs]"));
        assert!(!source.contains("[local_file: src/a.rs]"));
        assert!(!source.contains("[local_glob: src/**/*.rs]"));
        assert!(request.requested_prompt.contains("[local_image: src/a.rs]"));
        assert!(request.requested_prompt.contains("[local_file: src/a.rs]"));
        assert!(
            request
                .requested_prompt
                .contains("[local_glob: src/**/*.rs]")
        );
        app.shutdown_oracle_broker(true);
        Ok(())
    }

    #[tokio::test]
    async fn oracle_user_turn_attachment_resolution_failure_surfaces_error_without_starting_run()
    -> Result<()> {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let oracle_repo =
            find_oracle_repo(app.chat_widget.config_ref().cwd.as_path()).expect("oracle repo");
        app.oracle_broker = Some((oracle_repo, OracleBrokerClient::new_test_client()));
        let thread_id = app.ensure_visible_oracle_thread().await;
        while app_event_rx.try_recv().is_ok() {}

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let handled = app
            .handle_oracle_user_turn(
                &mut app_server,
                thread_id,
                &[UserInput::Text {
                    text: "Review this\nfile: src/missing.rs".to_string(),
                    text_elements: Vec::new(),
                }],
            )
            .await?;

        assert!(handled);
        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert!(app.oracle_state.active_run_id.is_none());
        assert!(app.oracle_state.inflight_run_requests.is_empty());
        let history = std::iter::from_fn(|| app_event_rx.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => {
                    Some(lines_to_single_string(&cell.display_lines(/*width*/ 120)))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(history.iter().any(|cell| {
            cell.contains("Oracle attachment setup failed:")
                && cell.contains("file src/missing.rs could not be read")
        }));
        Ok(())
    }

    #[tokio::test]
    async fn submit_thread_op_routes_visible_oracle_thread_user_turn_through_supervisor()
    -> Result<()> {
        let mut app = make_test_app().await;
        std::fs::create_dir_all(app.chat_widget.config_ref().cwd.as_path())?;
        let oracle_repo =
            find_oracle_repo(app.chat_widget.config_ref().cwd.as_path()).expect("oracle repo");
        app.oracle_broker = Some((oracle_repo, OracleBrokerClient::new_test_client()));
        let thread_id = app.ensure_visible_oracle_thread().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");

        app.submit_thread_op(
            &mut app_server,
            thread_id,
            AppCommand::user_turn(
                vec![UserInput::Text {
                    text: "Reply with exactly routed-through-oracle and nothing else.".to_string(),
                    text_elements: Vec::new(),
                }],
                app.chat_widget.config_ref().cwd.to_path_buf(),
                app.chat_widget
                    .config_ref()
                    .permissions
                    .approval_policy
                    .value(),
                app.chat_widget.config_ref().legacy_sandbox_policy(),
                Some(
                    app.chat_widget
                        .config_ref()
                        .permissions
                        .permission_profile(),
                ),
                app.oracle_thread_session(thread_id).model,
                None,
                None,
                app.chat_widget.config_ref().service_tier.map(Some),
                None,
                None,
                app.chat_widget.config_ref().personality,
            ),
        )
        .await?;

        assert_eq!(app.oracle_state.oracle_thread_id, Some(thread_id));
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn)
        );
        assert!(app.oracle_state.active_run_id.is_some());
        assert!(app.oracle_state.pending_turn_id.is_some());
        let request = app
            .oracle_state
            .inflight_run_requests
            .values()
            .next()
            .expect("oracle request");
        assert_eq!(request.oracle_thread_id, thread_id);
        assert_eq!(request.kind, OracleRequestKind::UserTurn);
        assert_eq!(
            request.source_user_text.as_deref(),
            Some("Reply with exactly routed-through-oracle and nothing else.")
        );
        app.shutdown_oracle_broker(true);
        Ok(())
    }

    #[tokio::test]
    async fn slash_oracle_on_entrypoint_reuses_the_same_visible_thread_as_natural_language_entrypoint()
    -> Result<()> {
        let mut app = make_test_app().await;
        let oracle_repo =
            find_oracle_repo(app.chat_widget.config_ref().cwd.as_path()).expect("oracle repo");
        app.oracle_broker = Some((oracle_repo, OracleBrokerClient::new_test_client()));
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");

        app.handle_configure_oracle_command(None, &mut app_server, "on")
            .await?;
        let oracle_thread_id = app.oracle_state.oracle_thread_id.expect("oracle thread");
        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(
            app.oracle_state.last_status.as_deref(),
            Some(
                format!(
                    "Oracle thread is enabled on {oracle_thread_id}. Requested model: {}.",
                    app.oracle_state.model.display_name()
                )
                .as_str()
            )
        );

        let handled = app
            .maybe_route_natural_language_oracle_invocation(
                None,
                &mut app_server,
                ThreadId::new(),
                &[UserInput::Text {
                    text: "Use your oracle skill to review the protocol changes.".to_string(),
                    text_elements: Vec::new(),
                }],
            )
            .await?;

        assert!(handled);
        assert_eq!(app.oracle_state.oracle_thread_id, Some(oracle_thread_id));
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn)
        );
        assert!(app.oracle_state.pending_turn_id.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn active_workflow_user_turn_requires_control_without_explicit_handoff_language()
    -> Result<()> {
        let mut app = make_test_app().await;
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 3,
            ..Default::default()
        });

        assert!(app.oracle_workflow_requires_control());
        assert!(!user_turn_requires_orchestrator_control(
            "continue the workflow"
        ));

        Ok(())
    }

    #[tokio::test]
    async fn oracle_thread_interrupt_aborts_active_user_turn_locally() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let oracle_repo =
            find_oracle_repo(app.chat_widget.config_ref().cwd.as_path()).expect("oracle repo");
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.active_run_id = Some("oracle-run-interrupt-user".to_string());
        app.oracle_state
            .inflight_runs
            .insert(thread_id, "oracle-run-interrupt-user".to_string());
        app.oracle_state.inflight_run_requests.insert(
            "oracle-run-interrupt-user".to_string(),
            OracleRunRequest {
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                session_slug: "oracle-session-root".to_string(),
                prompt: "hello oracle".to_string(),
                requested_prompt: "hello oracle".to_string(),
                source_user_text: Some("hello oracle".to_string()),
                files: Vec::new(),
                workspace_cwd: app.chat_widget.config_ref().cwd.to_path_buf(),
                oracle_repo,
                followup_session: Some("oracle-session-a".to_string()),
                model: OracleModelPreset::Pro,
                browser_model_strategy: "select".to_string(),
                browser_model_label: Some("GPT-5.5 Pro".to_string()),
                browser_thinking_time: Some("standard".to_string()),
                requires_control: false,
                repair_attempt: 0,
                transport_retry_attempt: 0,
            },
        );
        app.oracle_state.pending_turn_id = Some("oracle-turn-1".to_string());
        app.oracle_state.session_root_slug = Some("oracle-session-root".to_string());
        app.oracle_state.current_session_id = Some("oracle-session-a".to_string());
        app.oracle_state.current_session_ownership = Some(OracleSessionOwnership::SessionRootSlug(
            "oracle-session-root".to_string(),
        ));
        app.set_oracle_remote_metadata(
            thread_id,
            Some("conversation-before-interrupt".to_string()),
            Some("Oracle Thread".to_string()),
        );
        app.persist_active_oracle_binding();
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-1".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-1".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-1".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "hello oracle".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let handled = app
            .try_submit_active_thread_op_via_app_server(
                &mut app_server,
                thread_id,
                &AppCommand::interrupt(),
            )
            .await?;

        assert!(handled);
        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.active_run_id, None);
        assert_eq!(
            app.oracle_state.aborted_run_id.as_deref(),
            Some("oracle-run-interrupt-user")
        );
        assert_eq!(app.oracle_state.pending_turn_id, None);
        assert_eq!(app.oracle_state.current_session_id, None);
        assert_eq!(app.oracle_state.current_session_ownership, None);
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(binding.current_session_id, None);
        assert_eq!(binding.current_session_ownership, None);
        assert_eq!(binding.session_root_slug, None);
        assert_eq!(
            binding.conversation_id.as_deref(),
            Some("conversation-before-interrupt")
        );
        assert_eq!(binding.remote_title.as_deref(), Some("Oracle Thread"));
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert_eq!(store.active_turn_id, None);
        let turn = store
            .turns
            .iter()
            .find(|turn| turn.id == "oracle-turn-1")
            .expect("oracle turn");
        assert_eq!(turn.status, TurnStatus::Completed);
        assert!(store.buffer.iter().any(|event| matches!(
            event,
            ThreadBufferedEvent::OracleWorkflowEvent(OracleWorkflowThreadEvent { title, .. })
                if title == "Oracle request interrupted. The browser run was aborted."
        )));
        drop(store);

        app.handle_oracle_run_failed(
            &mut app_server,
            "oracle-run-interrupt-user".to_string(),
            thread_id,
            OracleRequestKind::UserTurn,
            "codex-oracle-interrupt-user".to_string(),
            "Oracle broker closed before replying".to_string(),
        )
        .await?;

        assert_eq!(app.oracle_state.aborted_run_id, None);
        assert!(app.oracle_state.aborted_runs.is_empty());
        assert!(app.oracle_state.inflight_run_requests.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn oracle_restart_after_interrupt_reattaches_the_same_remote_conversation() -> Result<()>
    {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let oracle_repo =
            find_oracle_repo(app.chat_widget.config_ref().cwd.as_path()).expect("oracle repo");
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::UserTurn);
        app.oracle_state.active_run_id = Some("oracle-run-interrupt-resume".to_string());
        app.oracle_state
            .inflight_runs
            .insert(thread_id, "oracle-run-interrupt-resume".to_string());
        app.oracle_state.inflight_run_requests.insert(
            "oracle-run-interrupt-resume".to_string(),
            OracleRunRequest {
                oracle_thread_id: thread_id,
                kind: OracleRequestKind::UserTurn,
                session_slug: "oracle-session-root".to_string(),
                prompt: "resume me".to_string(),
                requested_prompt: "resume me".to_string(),
                source_user_text: Some("resume me".to_string()),
                files: Vec::new(),
                workspace_cwd: app.chat_widget.config_ref().cwd.to_path_buf(),
                oracle_repo,
                followup_session: Some("oracle-session-a".to_string()),
                model: OracleModelPreset::Pro,
                browser_model_strategy: "select".to_string(),
                browser_model_label: Some("GPT-5.5 Pro".to_string()),
                browser_thinking_time: Some("standard".to_string()),
                requires_control: false,
                repair_attempt: 0,
                transport_retry_attempt: 0,
            },
        );
        app.oracle_state.pending_turn_id = Some("oracle-turn-resume".to_string());
        app.oracle_state.session_root_slug = Some("oracle-session-root".to_string());
        app.oracle_state.current_session_id = Some("oracle-session-a".to_string());
        app.oracle_state.current_session_ownership = Some(OracleSessionOwnership::SessionRootSlug(
            "oracle-session-root".to_string(),
        ));
        app.set_oracle_remote_metadata(
            thread_id,
            Some("conversation-before-interrupt".to_string()),
            Some("Oracle Thread".to_string()),
        );
        app.persist_active_oracle_binding();
        if let Some(channel) = app.thread_event_channels.get(&thread_id) {
            let mut store = channel.store.lock().await;
            store.active_turn_id = Some("oracle-turn-resume".to_string());
            store.turns.push(Turn {
                id: "oracle-turn-resume".to_string(),
                items: vec![ThreadItem::UserMessage {
                    id: "oracle-user-resume".to_string(),
                    content: vec![codex_app_server_protocol::UserInput::Text {
                        text: "resume me".to_string(),
                        text_elements: Vec::new(),
                    }],
                }],
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            });
        }

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let handled = app
            .try_submit_active_thread_op_via_app_server(
                &mut app_server,
                thread_id,
                &AppCommand::interrupt(),
            )
            .await?;
        assert!(handled);
        assert_eq!(
            app.oracle_state
                .bindings
                .get(&thread_id)
                .and_then(|binding| binding.conversation_id.as_deref()),
            Some("conversation-before-interrupt")
        );

        queue_test_oracle_attach_thread_result(
            &app,
            Ok(OracleBrokerThreadOpenResponse {
                session_id: Some("oracle-session-b".to_string()),
                title: "Oracle Thread".to_string(),
                conversation_id: Some("conversation-before-interrupt".to_string()),
                url: Some("https://chatgpt.com/c/conversation-before-interrupt".to_string()),
                history: Vec::new(),
            }),
        );

        let (_, followup_session) = app
            .ensure_oracle_session_continuity_for_run(thread_id)
            .await?;

        assert_eq!(followup_session.as_deref(), Some("oracle-session-b"));
        let hooks = app
            .oracle_test_broker_hooks
            .lock()
            .expect("oracle test broker hooks");
        assert_eq!(
            hooks.attach_thread_calls,
            vec!["conversation-before-interrupt".to_string()]
        );
        drop(hooks);
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(
            binding.conversation_id.as_deref(),
            Some("conversation-before-interrupt")
        );
        assert_eq!(
            binding.current_session_id.as_deref(),
            Some("oracle-session-b")
        );
        Ok(())
    }

    #[tokio::test]
    async fn oracle_thread_interrupt_aborts_active_checkpoint_review_locally() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let checkpoint_thread_id = ThreadId::new();
        app.oracle_state.phase =
            OracleSupervisorPhase::WaitingForOracle(OracleRequestKind::Checkpoint);
        app.oracle_state.active_run_id = Some("oracle-run-interrupt-checkpoint".to_string());
        app.oracle_state.active_checkpoint_thread_id = Some(checkpoint_thread_id);
        app.oracle_state.active_checkpoint_turn_id = Some("orchestrator-turn-1".to_string());
        app.oracle_state.session_root_slug = Some("oracle-session-root".to_string());
        app.oracle_state.current_session_id = Some("oracle-session-a".to_string());
        app.oracle_state.current_session_ownership = Some(OracleSessionOwnership::SessionRootSlug(
            "oracle-session-root".to_string(),
        ));
        app.persist_active_oracle_binding();

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let handled = app
            .try_submit_active_thread_op_via_app_server(
                &mut app_server,
                thread_id,
                &AppCommand::interrupt(),
            )
            .await?;

        assert!(handled);
        assert_eq!(app.oracle_state.phase, OracleSupervisorPhase::Idle);
        assert_eq!(app.oracle_state.active_run_id, None);
        assert_eq!(
            app.oracle_state.aborted_run_id.as_deref(),
            Some("oracle-run-interrupt-checkpoint")
        );
        assert_eq!(app.oracle_state.current_session_id, None);
        assert_eq!(app.oracle_state.current_session_ownership, None);
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(binding.current_session_id, None);
        assert_eq!(binding.current_session_ownership, None);
        assert_eq!(binding.session_root_slug, None);
        assert_eq!(
            binding.pending_checkpoint_threads.first().copied(),
            Some(checkpoint_thread_id)
        );
        assert_eq!(
            binding
                .pending_checkpoint_versions
                .get(&checkpoint_thread_id),
            None
        );
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("oracle thread channel");
        let store = channel.store.lock().await;
        assert!(store.buffer.iter().any(|event| matches!(
            event,
            ThreadBufferedEvent::OracleWorkflowEvent(OracleWorkflowThreadEvent { title, .. })
                if title == "Oracle checkpoint review interrupted. The browser run was aborted."
        )));
        Ok(())
    }

    #[tokio::test]
    async fn interrupt_from_hidden_orchestrator_thread_preserves_workflow_when_backend_interrupt_is_not_confirmed()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.phase = OracleSupervisorPhase::WaitingForOrchestrator;
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 1,
            orchestrator_thread_id: Some(orchestrator_thread_id),
            ..Default::default()
        });
        if let Some(binding) = app.oracle_state.bindings.get_mut(&thread_id) {
            binding.participants.insert(
                ORACLE_ORCHESTRATOR_DESTINATION.to_string(),
                OracleRoutingParticipant {
                    address: ORACLE_ORCHESTRATOR_DESTINATION.to_string(),
                    thread_id: Some(orchestrator_thread_id),
                    title: Some("orchestrator".to_string()),
                    kind: Some("orchestrator".to_string()),
                    role: Some("orchestrator".to_string()),
                    visibility: OracleParticipantVisibility::Hidden,
                    owned_by_oracle: true,
                    route_completions: true,
                    route_closures: true,
                    last_task: None,
                },
            );
        }
        app.persist_active_oracle_binding();

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let handled = app
            .try_submit_active_thread_op_via_app_server(
                &mut app_server,
                orchestrator_thread_id,
                &AppCommand::interrupt(),
            )
            .await?;

        assert!(handled);
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOrchestrator
        );
        assert_eq!(
            app.find_oracle_thread_by_orchestrator_thread(orchestrator_thread_id),
            Some(thread_id)
        );
        Ok(())
    }

    #[tokio::test]
    async fn oracle_thread_interrupt_while_waiting_preserves_workflow_when_backend_interrupt_is_not_confirmed()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        let orchestrator_thread_id = ThreadId::new();
        app.oracle_state.phase = OracleSupervisorPhase::WaitingForOrchestrator;
        app.oracle_state.orchestrator_thread_id = Some(orchestrator_thread_id);
        app.oracle_state.workflow = Some(OracleWorkflowBinding {
            workflow_id: "oracle-routing".to_string(),
            mode: OracleWorkflowMode::Supervising,
            status: OracleWorkflowStatus::Running,
            version: 1,
            orchestrator_thread_id: Some(orchestrator_thread_id),
            ..Default::default()
        });
        if let Some(binding) = app.oracle_state.bindings.get_mut(&thread_id) {
            binding.participants.insert(
                ORACLE_ORCHESTRATOR_DESTINATION.to_string(),
                OracleRoutingParticipant {
                    address: ORACLE_ORCHESTRATOR_DESTINATION.to_string(),
                    thread_id: Some(orchestrator_thread_id),
                    title: Some("orchestrator".to_string()),
                    kind: Some("orchestrator".to_string()),
                    role: Some("orchestrator".to_string()),
                    visibility: OracleParticipantVisibility::Hidden,
                    owned_by_oracle: true,
                    route_completions: true,
                    route_closures: true,
                    last_task: None,
                },
            );
        }
        app.persist_active_oracle_binding();

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let handled = app
            .try_submit_active_thread_op_via_app_server(
                &mut app_server,
                thread_id,
                &AppCommand::interrupt(),
            )
            .await?;

        assert!(handled);
        assert_eq!(
            app.oracle_state.phase,
            OracleSupervisorPhase::WaitingForOrchestrator
        );
        assert_eq!(
            app.oracle_state.orchestrator_thread_id,
            Some(orchestrator_thread_id)
        );
        let binding = app
            .oracle_state
            .bindings
            .get(&thread_id)
            .expect("oracle binding");
        assert_eq!(binding.orchestrator_thread_id, Some(orchestrator_thread_id));
        assert_eq!(
            binding
                .workflow
                .as_ref()
                .and_then(|workflow| workflow.orchestrator_thread_id),
            Some(orchestrator_thread_id)
        );
        assert_eq!(
            app.find_oracle_thread_by_orchestrator_thread(orchestrator_thread_id),
            Some(thread_id)
        );
        assert!(binding.last_status.as_deref().is_some_and(|status| {
            status.contains("could not confirm a backend interrupt")
                || status.contains("workflow remains attached")
        }));
        Ok(())
    }

    #[tokio::test]
    async fn idle_oracle_thread_does_not_swallow_interrupt() -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = app.ensure_visible_oracle_thread().await;
        app.oracle_state.phase = OracleSupervisorPhase::Idle;

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let handled = app
            .try_submit_active_thread_op_via_app_server(
                &mut app_server,
                thread_id,
                &AppCommand::interrupt(),
            )
            .await?;

        assert!(!handled);
        Ok(())
    }

    #[tokio::test]
    async fn refresh_pending_thread_approvals_only_lists_inactive_threads() {
        let mut app = make_test_app().await;
        let main_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000001").expect("valid thread");
        let agent_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000002").expect("valid thread");

        app.primary_thread_id = Some(main_thread_id);
        app.active_thread_id = Some(main_thread_id);
        app.thread_event_channels
            .insert(main_thread_id, ThreadEventChannel::new(/*capacity*/ 1));

        let agent_channel = ThreadEventChannel::new(/*capacity*/ 1);
        {
            let mut store = agent_channel.store.lock().await;
            store.push_request(exec_approval_request(
                agent_thread_id,
                "turn-1",
                "call-1",
                /*approval_id*/ None,
            ));
        }
        app.thread_event_channels
            .insert(agent_thread_id, agent_channel);
        app.agent_navigation.upsert(
            agent_thread_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            /*is_closed*/ false,
        );

        app.refresh_pending_thread_approvals().await;
        assert_eq!(
            app.chat_widget.pending_thread_approvals(),
            &["Robie [explorer]".to_string()]
        );

        app.active_thread_id = Some(agent_thread_id);
        app.refresh_pending_thread_approvals().await;
        assert!(app.chat_widget.pending_thread_approvals().is_empty());
    }

    #[tokio::test]
    async fn inactive_thread_approval_bubbles_into_active_view() -> Result<()> {
        let mut app = make_test_app().await;
        let main_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000011").expect("valid thread");
        let agent_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000022").expect("valid thread");

        app.primary_thread_id = Some(main_thread_id);
        app.active_thread_id = Some(main_thread_id);
        app.thread_event_channels
            .insert(main_thread_id, ThreadEventChannel::new(/*capacity*/ 1));
        app.thread_event_channels.insert(
            agent_thread_id,
            ThreadEventChannel::new_with_session(
                /*capacity*/ 1,
                ThreadSessionState {
                    approval_policy: AskForApproval::OnRequest,
                    sandbox_policy: SandboxPolicy::new_workspace_write_policy(),
                    rollout_path: Some(test_path_buf("/tmp/agent-rollout.jsonl")),
                    ..test_thread_session(agent_thread_id, test_path_buf("/tmp/agent"))
                },
                Vec::new(),
            ),
        );
        app.agent_navigation.upsert(
            agent_thread_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            /*is_closed*/ false,
        );

        app.enqueue_thread_request(
            agent_thread_id,
            exec_approval_request(
                agent_thread_id,
                "turn-approval",
                "call-approval",
                /*approval_id*/ None,
            ),
        )
        .await?;

        assert_eq!(app.chat_widget.has_active_view(), true);
        assert_eq!(
            app.chat_widget.pending_thread_approvals(),
            &["Robie [explorer]".to_string()]
        );

        Ok(())
    }

    #[tokio::test]
    async fn side_defers_parent_approval_overlay_until_parent_replay() -> Result<()> {
        let mut app = make_test_app().await;
        let parent_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000011").expect("valid thread");
        let side_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000022").expect("valid thread");

        app.primary_thread_id = Some(parent_thread_id);
        app.active_thread_id = Some(side_thread_id);
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(parent_thread_id));
        app.thread_event_channels.insert(
            parent_thread_id,
            ThreadEventChannel::new_with_session(
                /*capacity*/ 4,
                test_thread_session(parent_thread_id, test_path_buf("/tmp/main")),
                Vec::new(),
            ),
        );

        app.enqueue_thread_request(
            parent_thread_id,
            exec_approval_request(
                parent_thread_id,
                "turn-approval",
                "call-approval",
                /*approval_id*/ None,
            ),
        )
        .await?;

        assert_eq!(app.chat_widget.has_active_view(), false);
        assert!(app.chat_widget.pending_thread_approvals().is_empty());
        assert_eq!(
            app.side_threads
                .get(&side_thread_id)
                .and_then(|state| state.parent_status),
            Some(SideParentStatus::NeedsApproval)
        );

        let snapshot = {
            let channel = app
                .thread_event_channels
                .get(&parent_thread_id)
                .expect("parent thread channel");
            let store = channel.store.lock().await;
            store.snapshot()
        };
        app.side_threads.remove(&side_thread_id);
        app.active_thread_id = Some(parent_thread_id);
        app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);

        assert_eq!(app.chat_widget.has_active_view(), true);

        Ok(())
    }

    #[tokio::test]
    async fn replay_snapshot_with_pending_request_suppresses_replay_notices() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000011").expect("valid thread");
        let stale_warning = "stale startup warning that should not cover the approval";

        app.replay_thread_snapshot(
            ThreadEventSnapshot {
                session: Some(test_thread_session(thread_id, test_path_buf("/tmp/main"))),
                turns: Vec::new(),
                events: vec![
                    ThreadBufferedEvent::Notification(ServerNotification::Warning(
                        WarningNotification {
                            thread_id: Some(thread_id.to_string()),
                            message: stale_warning.to_string(),
                        },
                    )),
                    ThreadBufferedEvent::Request(exec_approval_request(
                        thread_id,
                        "turn-approval",
                        "call-approval",
                        /*approval_id*/ None,
                    )),
                ],
                replay_entries: Vec::new(),
                input_state: None,
            },
            /*resume_restored_queue*/ false,
        );

        assert_eq!(app.chat_widget.has_active_view(), true);

        let mut replayed_history = String::new();
        while let Ok(event) = app_event_rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                replayed_history.push_str(&lines_to_single_string(
                    &cell.transcript_lines(/*width*/ 80),
                ));
            }
        }

        assert!(
            replayed_history.is_empty(),
            "expected pending approval replay to suppress session notices, got {replayed_history:?}"
        );
    }

    #[tokio::test]
    async fn side_defers_subagent_approval_overlay_until_side_exits() -> Result<()> {
        let mut app = make_test_app().await;
        let main_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000011").expect("valid thread");
        let side_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000022").expect("valid thread");
        let agent_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000033").expect("valid thread");

        app.primary_thread_id = Some(main_thread_id);
        app.active_thread_id = Some(side_thread_id);
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(main_thread_id));
        app.thread_event_channels.insert(
            agent_thread_id,
            ThreadEventChannel::new_with_session(
                /*capacity*/ 4,
                ThreadSessionState {
                    approval_policy: AskForApproval::OnRequest,
                    sandbox_policy: SandboxPolicy::new_workspace_write_policy(),
                    rollout_path: Some(test_path_buf("/tmp/agent-rollout.jsonl")),
                    ..test_thread_session(agent_thread_id, test_path_buf("/tmp/agent"))
                },
                Vec::new(),
            ),
        );
        app.agent_navigation.upsert(
            agent_thread_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            /*is_closed*/ false,
        );

        app.enqueue_thread_request(
            agent_thread_id,
            exec_approval_request(
                agent_thread_id,
                "turn-approval",
                "call-approval",
                /*approval_id*/ None,
            ),
        )
        .await?;

        assert_eq!(app.chat_widget.has_active_view(), false);
        assert_eq!(
            app.chat_widget.pending_thread_approvals(),
            &["Robie [explorer]".to_string()]
        );

        app.side_threads.remove(&side_thread_id);
        app.active_thread_id = Some(main_thread_id);
        app.surface_pending_inactive_thread_interactive_requests()
            .await;

        assert_eq!(app.chat_widget.has_active_view(), true);

        Ok(())
    }

    #[tokio::test]
    async fn inactive_thread_exec_approval_preserves_context() {
        let app = make_test_app().await;
        let thread_id = ThreadId::new();
        let mut request = exec_approval_request(
            thread_id,
            "turn-approval",
            "call-approval",
            /*approval_id*/ None,
        );
        let ServerRequest::CommandExecutionRequestApproval { params, .. } = &mut request else {
            panic!("expected exec approval request");
        };
        params.network_approval_context = Some(AppServerNetworkApprovalContext {
            host: "example.com".to_string(),
            protocol: AppServerNetworkApprovalProtocol::Socks5Tcp,
        });
        params.additional_permissions = Some(AdditionalPermissionProfile {
            network: Some(AdditionalNetworkPermissions {
                enabled: Some(true),
            }),
            file_system: Some(AdditionalFileSystemPermissions {
                read: Some(vec![test_absolute_path("/tmp/read-only")]),
                write: Some(vec![test_absolute_path("/tmp/write")]),
                glob_scan_max_depth: None,
                entries: None,
            }),
        });
        params.proposed_network_policy_amendments = Some(vec![AppServerNetworkPolicyAmendment {
            host: "example.com".to_string(),
            action: AppServerNetworkPolicyRuleAction::Allow,
        }]);

        let Some(ThreadInteractiveRequest::Approval(ApprovalRequest::Exec {
            available_decisions,
            network_approval_context,
            additional_permissions,
            ..
        })) = app
            .interactive_request_for_thread_request(thread_id, &request)
            .await
        else {
            panic!("expected exec approval request");
        };

        assert_eq!(
            network_approval_context,
            Some(NetworkApprovalContext {
                host: "example.com".to_string(),
                protocol: NetworkApprovalProtocol::Socks5Tcp,
            })
        );
        assert_eq!(
            additional_permissions,
            Some(codex_protocol::models::AdditionalPermissionProfile {
                network: Some(NetworkPermissions {
                    enabled: Some(true),
                }),
                file_system: Some(FileSystemPermissions::from_read_write_roots(
                    Some(vec![test_absolute_path("/tmp/read-only")]),
                    Some(vec![test_absolute_path("/tmp/write")]),
                )),
            })
        );
        assert_eq!(
            available_decisions,
            vec![
                codex_protocol::protocol::ReviewDecision::Approved,
                codex_protocol::protocol::ReviewDecision::ApprovedForSession,
                codex_protocol::protocol::ReviewDecision::NetworkPolicyAmendment {
                    network_policy_amendment: codex_protocol::approvals::NetworkPolicyAmendment {
                        host: "example.com".to_string(),
                        action: codex_protocol::approvals::NetworkPolicyRuleAction::Allow,
                    },
                },
                codex_protocol::protocol::ReviewDecision::Abort,
            ]
        );
    }

    #[tokio::test]
    async fn inactive_thread_exec_approval_splits_shell_wrapped_command() {
        let app = make_test_app().await;
        let thread_id = ThreadId::new();
        let script = r#"python3 -c 'print("Hello, world!")'"#;
        let mut request = exec_approval_request(
            thread_id,
            "turn-approval",
            "call-approval",
            /*approval_id*/ None,
        );
        let ServerRequest::CommandExecutionRequestApproval { params, .. } = &mut request else {
            panic!("expected exec approval request");
        };
        params.command = Some(
            shlex::try_join(["/bin/zsh", "-lc", script]).expect("round-trippable shell wrapper"),
        );

        let Some(ThreadInteractiveRequest::Approval(ApprovalRequest::Exec { command, .. })) = app
            .interactive_request_for_thread_request(thread_id, &request)
            .await
        else {
            panic!("expected exec approval request");
        };

        assert_eq!(
            command,
            vec![
                "/bin/zsh".to_string(),
                "-lc".to_string(),
                script.to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn inactive_thread_permissions_approval_preserves_file_system_permissions() {
        let app = make_test_app().await;
        let thread_id = ThreadId::new();
        let request = ServerRequest::PermissionsRequestApproval {
            request_id: AppServerRequestId::Integer(7),
            params: PermissionsRequestApprovalParams {
                thread_id: thread_id.to_string(),
                turn_id: "turn-approval".to_string(),
                item_id: "call-approval".to_string(),
                cwd: test_absolute_path("/tmp"),
                reason: Some("Need access to .git".to_string()),
                permissions: codex_app_server_protocol::RequestPermissionProfile {
                    network: Some(AdditionalNetworkPermissions {
                        enabled: Some(true),
                    }),
                    file_system: Some(AdditionalFileSystemPermissions {
                        read: Some(vec![test_absolute_path("/tmp/read-only")]),
                        write: Some(vec![test_absolute_path("/tmp/write")]),
                        glob_scan_max_depth: None,
                        entries: None,
                    }),
                },
            },
        };

        let Some(ThreadInteractiveRequest::Approval(ApprovalRequest::Permissions {
            permissions,
            ..
        })) = app
            .interactive_request_for_thread_request(thread_id, &request)
            .await
        else {
            panic!("expected permissions approval request");
        };

        assert_eq!(
            permissions,
            RequestPermissionProfile {
                network: Some(NetworkPermissions {
                    enabled: Some(true),
                }),
                file_system: Some(FileSystemPermissions::from_read_write_roots(
                    Some(vec![test_absolute_path("/tmp/read-only")]),
                    Some(vec![test_absolute_path("/tmp/write")]),
                )),
            }
        );
    }

    #[tokio::test]
    async fn inactive_thread_approval_badge_clears_after_turn_completion_notification() -> Result<()>
    {
        let mut app = make_test_app().await;
        let main_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000101").expect("valid thread");
        let agent_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000202").expect("valid thread");

        app.primary_thread_id = Some(main_thread_id);
        app.active_thread_id = Some(main_thread_id);
        app.thread_event_channels
            .insert(main_thread_id, ThreadEventChannel::new(/*capacity*/ 1));
        app.thread_event_channels.insert(
            agent_thread_id,
            ThreadEventChannel::new_with_session(
                /*capacity*/ 4,
                ThreadSessionState {
                    approval_policy: AskForApproval::OnRequest,
                    sandbox_policy: SandboxPolicy::new_workspace_write_policy(),
                    rollout_path: Some(test_path_buf("/tmp/agent-rollout.jsonl")),
                    ..test_thread_session(agent_thread_id, test_path_buf("/tmp/agent"))
                },
                Vec::new(),
            ),
        );
        app.agent_navigation.upsert(
            agent_thread_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            /*is_closed*/ false,
        );

        app.enqueue_thread_request(
            agent_thread_id,
            exec_approval_request(
                agent_thread_id,
                "turn-approval",
                "call-approval",
                /*approval_id*/ None,
            ),
        )
        .await?;
        assert_eq!(
            app.chat_widget.pending_thread_approvals(),
            &["Robie [explorer]".to_string()]
        );

        app.enqueue_thread_notification(
            agent_thread_id,
            turn_completed_notification(agent_thread_id, "turn-approval", TurnStatus::Completed),
        )
        .await?;

        assert!(
            app.chat_widget.pending_thread_approvals().is_empty(),
            "turn completion should clear inactive-thread approval badge immediately"
        );

        Ok(())
    }

    #[tokio::test]
    async fn inactive_thread_started_notification_initializes_replay_session() -> Result<()> {
        let mut app = make_test_app().await;
        let temp_dir = tempdir()?;
        let main_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000101").expect("valid thread");
        let agent_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000202").expect("valid thread");
        let primary_session = ThreadSessionState {
            approval_policy: AskForApproval::OnRequest,
            sandbox_policy: SandboxPolicy::new_workspace_write_policy(),
            ..test_thread_session(main_thread_id, test_path_buf("/tmp/main"))
        };

        app.primary_thread_id = Some(main_thread_id);
        app.active_thread_id = Some(main_thread_id);
        app.primary_session_configured = Some(primary_session.clone());
        app.thread_event_channels.insert(
            main_thread_id,
            ThreadEventChannel::new_with_session(
                /*capacity*/ 4,
                primary_session.clone(),
                Vec::new(),
            ),
        );

        let rollout_path = temp_dir.path().join("agent-rollout.jsonl");
        let turn_context = TurnContextItem {
            turn_id: None,
            trace_id: None,
            cwd: test_path_buf("/tmp/agent"),
            current_date: None,
            timezone: None,
            approval_policy: primary_session.approval_policy,
            sandbox_policy: primary_session.sandbox_policy.clone(),
            permission_profile: None,
            network: None,
            file_system_sandbox_policy: None,
            model: "gpt-agent".to_string(),
            personality: None,
            collaboration_mode: None,
            realtime_active: Some(false),
            effort: primary_session.reasoning_effort,
            summary: app.config.model_reasoning_summary.unwrap_or_default(),
            user_instructions: None,
            developer_instructions: None,
            final_output_json_schema: None,
            truncation_policy: None,
        };
        let rollout = RolloutLine {
            timestamp: "t0".to_string(),
            item: RolloutItem::TurnContext(turn_context),
        };
        std::fs::write(
            &rollout_path,
            format!("{}\n", serde_json::to_string(&rollout)?),
        )?;
        app.enqueue_thread_notification(
            agent_thread_id,
            ServerNotification::ThreadStarted(ThreadStartedNotification {
                thread: Thread {
                    id: agent_thread_id.to_string(),
                    forked_from_id: None,
                    preview: "agent thread".to_string(),
                    ephemeral: false,
                    model_provider: "agent-provider".to_string(),
                    created_at: 1,
                    updated_at: 2,
                    status: codex_app_server_protocol::ThreadStatus::Idle,
                    path: Some(rollout_path.clone()),
                    cwd: test_path_buf("/tmp/agent").abs(),
                    cli_version: "0.0.0".to_string(),
                    source: codex_app_server_protocol::SessionSource::Unknown,
                    agent_nickname: Some("Robie".to_string()),
                    agent_role: Some("explorer".to_string()),
                    git_info: None,
                    name: Some("agent thread".to_string()),
                    turns: Vec::new(),
                },
            }),
        )
        .await?;

        let store = app
            .thread_event_channels
            .get(&agent_thread_id)
            .expect("agent thread channel")
            .store
            .lock()
            .await;
        let session = store.session.clone().expect("inferred session");
        drop(store);

        assert_eq!(session.thread_id, agent_thread_id);
        assert_eq!(session.thread_name, Some("agent thread".to_string()));
        assert_eq!(session.model, "gpt-agent");
        assert_eq!(session.model_provider_id, "agent-provider");
        assert_eq!(session.approval_policy, primary_session.approval_policy);
        assert_eq!(session.cwd.as_path(), test_path_buf("/tmp/agent").as_path());
        assert_eq!(session.rollout_path, Some(rollout_path));
        assert_eq!(
            app.agent_navigation.get(&agent_thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("explorer".to_string()),
                is_closed: false,
            })
        );

        Ok(())
    }

    #[tokio::test]
    async fn inactive_thread_started_notification_preserves_primary_model_when_path_missing()
    -> Result<()> {
        let mut app = make_test_app().await;
        let main_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000301").expect("valid thread");
        let agent_thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000302").expect("valid thread");
        let primary_session = ThreadSessionState {
            approval_policy: AskForApproval::OnRequest,
            sandbox_policy: SandboxPolicy::new_workspace_write_policy(),
            ..test_thread_session(main_thread_id, test_path_buf("/tmp/main"))
        };

        app.primary_thread_id = Some(main_thread_id);
        app.active_thread_id = Some(main_thread_id);
        app.primary_session_configured = Some(primary_session.clone());
        app.thread_event_channels.insert(
            main_thread_id,
            ThreadEventChannel::new_with_session(
                /*capacity*/ 4,
                primary_session.clone(),
                Vec::new(),
            ),
        );

        app.enqueue_thread_notification(
            agent_thread_id,
            ServerNotification::ThreadStarted(ThreadStartedNotification {
                thread: Thread {
                    id: agent_thread_id.to_string(),
                    forked_from_id: None,
                    preview: "agent thread".to_string(),
                    ephemeral: false,
                    model_provider: "agent-provider".to_string(),
                    created_at: 1,
                    updated_at: 2,
                    status: codex_app_server_protocol::ThreadStatus::Idle,
                    path: None,
                    cwd: test_path_buf("/tmp/agent").abs(),
                    cli_version: "0.0.0".to_string(),
                    source: codex_app_server_protocol::SessionSource::Unknown,
                    agent_nickname: Some("Robie".to_string()),
                    agent_role: Some("explorer".to_string()),
                    git_info: None,
                    name: Some("agent thread".to_string()),
                    turns: Vec::new(),
                },
            }),
        )
        .await?;

        let store = app
            .thread_event_channels
            .get(&agent_thread_id)
            .expect("agent thread channel")
            .store
            .lock()
            .await;
        let session = store.session.clone().expect("inferred session");

        assert_eq!(session.model, primary_session.model);

        Ok(())
    }

    #[test]
    fn agent_picker_item_name_snapshot() {
        let thread_id =
            ThreadId::from_string("00000000-0000-0000-0000-000000000123").expect("valid thread id");
        let snapshot = [
            format!(
                "{} | {}",
                format_agent_picker_item_name(
                    Some("Robie"),
                    Some("explorer"),
                    /*is_primary*/ true
                ),
                thread_id
            ),
            format!(
                "{} | {}",
                format_agent_picker_item_name(
                    Some("Robie"),
                    Some("explorer"),
                    /*is_primary*/ false
                ),
                thread_id
            ),
            format!(
                "{} | {}",
                format_agent_picker_item_name(
                    Some("Robie"),
                    /*agent_role*/ None,
                    /*is_primary*/ false
                ),
                thread_id
            ),
            format!(
                "{} | {}",
                format_agent_picker_item_name(
                    /*agent_nickname*/ None,
                    Some("explorer"),
                    /*is_primary*/ false
                ),
                thread_id
            ),
            format!(
                "{} | {}",
                format_agent_picker_item_name(
                    /*agent_nickname*/ None, /*agent_role*/ None,
                    /*is_primary*/ false
                ),
                thread_id
            ),
        ]
        .join("\n");
        assert_snapshot!("agent_picker_item_name", snapshot);
    }

    #[tokio::test]
    async fn side_fork_config_is_ephemeral_and_appends_developer_guardrails() {
        let mut app = make_test_app().await;
        app.config.developer_instructions = Some("Existing developer policy.".to_string());
        let original_approval_policy = app.config.permissions.approval_policy.value();
        let original_sandbox_policy = app.config.legacy_sandbox_policy();

        let fork_config = app.side_fork_config();

        assert!(fork_config.ephemeral);
        assert_eq!(
            fork_config.permissions.approval_policy.value(),
            original_approval_policy
        );
        assert_eq!(fork_config.legacy_sandbox_policy(), original_sandbox_policy);
        let developer_instructions = fork_config
            .developer_instructions
            .as_deref()
            .expect("side developer instructions");
        assert!(developer_instructions.contains("Existing developer policy."));
        assert!(
            developer_instructions.contains("You are in a side conversation, not the main thread.")
        );
        assert!(
            developer_instructions
                .contains("inherited fork history is provided only as reference context")
        );
        assert!(developer_instructions.contains(
            "Only instructions submitted after the side-conversation boundary are active"
        ));
        assert!(developer_instructions.contains("Do not continue, execute, or complete any task"));
        assert!(
            developer_instructions
                .contains("External tools may be available according to this thread's current")
        );
        assert!(
            developer_instructions
                .contains("Any MCP or external tool calls or outputs visible in the inherited")
        );
        assert!(developer_instructions.contains("non-mutating inspection"));
        assert!(developer_instructions.contains("Do not modify files"));
        assert!(developer_instructions.contains("Do not request escalated permissions"));
        assert!(app.transcript_cells.is_empty());
    }

    #[test]
    fn side_boundary_prompt_marks_inherited_history_reference_only() {
        let item = App::side_boundary_prompt_item();
        let codex_protocol::models::ResponseItem::Message { role, content, .. } = item else {
            panic!("expected hidden side boundary prompt to be a user message");
        };
        assert_eq!(role, "user");
        let [codex_protocol::models::ContentItem::InputText { text }] = content.as_slice() else {
            panic!("expected hidden side boundary prompt text");
        };
        assert!(text.contains("Side conversation boundary."));
        assert!(text.contains("Everything before this boundary is inherited history"));
        assert!(text.contains("It is not your current task."));
        assert!(text.contains("Only messages submitted after this boundary are active"));
        assert!(text.contains("Do not continue, execute, or complete"));
        assert!(text.contains("separate from the main thread"));
        assert!(
            text.contains("External tools may be available according to this thread's current")
        );
        assert!(text.contains("Any tool calls or outputs visible before this boundary happened"));
        assert!(text.contains("Do not modify files"));
    }

    #[test]
    fn side_return_shortcuts_match_esc_and_ctrl_c() {
        assert!(side_return_shortcut_matches(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )));
        assert!(side_return_shortcut_matches(KeyEvent::new_with_kind(
            KeyCode::Esc,
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        )));
        assert!(side_return_shortcut_matches(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert!(side_return_shortcut_matches(KeyEvent::new(
            KeyCode::Char('C'),
            KeyModifiers::CONTROL,
        )));
        assert!(!side_return_shortcut_matches(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        )));
        assert!(!side_return_shortcut_matches(KeyEvent::new_with_kind(
            KeyCode::Esc,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )));
    }

    #[tokio::test]
    async fn side_start_block_message_tracks_open_side_conversation() {
        let mut app = make_test_app().await;
        assert_eq!(
            app.side_start_block_message(),
            Some("'/side' is unavailable until the main thread is ready.")
        );

        app.primary_thread_id = Some(ThreadId::new());
        assert_eq!(app.side_start_block_message(), None);

        let parent_thread_id = ThreadId::new();
        let side_thread_id = ThreadId::new();
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(parent_thread_id));

        assert_eq!(
            app.side_start_block_message(),
            Some(
                "A side conversation is already open. Press Esc to return before starting another."
            )
        );

        app.side_threads.remove(&side_thread_id);
        assert_eq!(app.side_start_block_message(), None);
    }

    #[tokio::test]
    async fn side_parent_status_tracks_parent_turn_lifecycle() -> Result<()> {
        let mut app = make_test_app().await;
        let parent_thread_id = ThreadId::new();
        let side_thread_id = ThreadId::new();
        app.primary_thread_id = Some(parent_thread_id);
        app.active_thread_id = Some(side_thread_id);
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(parent_thread_id));

        app.enqueue_thread_notification(
            parent_thread_id,
            turn_completed_notification(parent_thread_id, "turn-1", TurnStatus::Completed),
        )
        .await?;
        assert_eq!(
            app.side_threads
                .get(&side_thread_id)
                .and_then(|state| state.parent_status),
            Some(SideParentStatus::Finished)
        );

        app.enqueue_thread_notification(
            parent_thread_id,
            turn_started_notification(parent_thread_id, "turn-2"),
        )
        .await?;
        assert_eq!(
            app.side_threads
                .get(&side_thread_id)
                .and_then(|state| state.parent_status),
            None
        );

        app.enqueue_thread_notification(
            parent_thread_id,
            turn_completed_notification(parent_thread_id, "turn-2", TurnStatus::Failed),
        )
        .await?;
        assert_eq!(
            app.side_threads
                .get(&side_thread_id)
                .and_then(|state| state.parent_status),
            Some(SideParentStatus::Failed)
        );

        Ok(())
    }

    #[tokio::test]
    async fn side_parent_status_prioritizes_input_over_approval() -> Result<()> {
        let mut app = make_test_app().await;
        let parent_thread_id = ThreadId::new();
        let side_thread_id = ThreadId::new();
        app.primary_thread_id = Some(parent_thread_id);
        app.active_thread_id = Some(side_thread_id);
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(parent_thread_id));

        app.enqueue_thread_request(
            parent_thread_id,
            exec_approval_request(
                parent_thread_id,
                "turn-approval",
                "call-approval",
                /*approval_id*/ None,
            ),
        )
        .await?;
        assert_eq!(
            app.side_threads
                .get(&side_thread_id)
                .and_then(|state| state.parent_status),
            Some(SideParentStatus::NeedsApproval)
        );

        app.enqueue_thread_request(
            parent_thread_id,
            request_user_input_request(parent_thread_id, "turn-input", "call-input"),
        )
        .await?;
        assert_eq!(
            app.side_threads
                .get(&side_thread_id)
                .and_then(|state| state.parent_status),
            Some(SideParentStatus::NeedsInput)
        );

        app.enqueue_thread_notification(
            parent_thread_id,
            ServerNotification::ServerRequestResolved(
                codex_app_server_protocol::ServerRequestResolvedNotification {
                    thread_id: parent_thread_id.to_string(),
                    request_id: AppServerRequestId::Integer(2),
                },
            ),
        )
        .await?;
        assert_eq!(
            app.side_threads
                .get(&side_thread_id)
                .and_then(|state| state.parent_status),
            Some(SideParentStatus::NeedsApproval)
        );

        app.enqueue_thread_notification(
            parent_thread_id,
            ServerNotification::ServerRequestResolved(
                codex_app_server_protocol::ServerRequestResolvedNotification {
                    thread_id: parent_thread_id.to_string(),
                    request_id: AppServerRequestId::Integer(1),
                },
            ),
        )
        .await?;
        assert_eq!(
            app.side_threads
                .get(&side_thread_id)
                .and_then(|state| state.parent_status),
            None
        );

        Ok(())
    }

    #[test]
    fn side_start_error_message_explains_missing_first_prompt() {
        let err = color_eyre::eyre::eyre!(
            "thread/fork failed during TUI bootstrap: thread/fork failed: no rollout found for thread id 019da1a1-bed9-7a43-88a2-b49d43915021"
        );

        assert_eq!(
            App::side_start_error_message(&err),
            "'/side' is unavailable until the current conversation has started. Send a message first, then try /side again."
        );
    }

    #[test]
    fn side_start_error_message_uses_generic_start_wording() {
        let err = color_eyre::eyre::eyre!("transport disconnected");

        assert_eq!(
            App::side_start_error_message(&err),
            "Failed to start side conversation: transport disconnected"
        );
    }

    #[tokio::test]
    async fn side_thread_snapshot_hides_forked_parent_transcript() {
        let parent_thread_id = ThreadId::new();
        let side_thread_id = ThreadId::new();
        let mut store = ThreadEventStore::new(/*capacity*/ 4);
        let session = ThreadSessionState {
            forked_from_id: Some(parent_thread_id),
            ..test_thread_session(side_thread_id, test_path_buf("/tmp/side"))
        };
        let parent_turn = test_turn(
            "parent-turn",
            TurnStatus::Completed,
            vec![ThreadItem::UserMessage {
                id: "parent-user".to_string(),
                content: vec![AppServerUserInput::Text {
                    text: "parent prompt should stay hidden".to_string(),
                    text_elements: Vec::new(),
                }],
            }],
        );

        App::install_side_thread_snapshot(&mut store, session, vec![parent_turn]);

        let stored_session = store.session.as_ref().expect("side session");
        assert_eq!(stored_session.thread_id, side_thread_id);
        assert_eq!(stored_session.forked_from_id, None);
        assert_eq!(store.turns, Vec::<Turn>::new());
        assert_eq!(store.active_turn_id(), None);
    }

    #[tokio::test]
    async fn side_thread_snapshot_does_not_refresh_from_fork_history() {
        let mut app = make_test_app().await;
        let parent_thread_id = ThreadId::new();
        let side_thread_id = ThreadId::new();
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(parent_thread_id));

        let snapshot = ThreadEventSnapshot {
            session: Some(ThreadSessionState {
                rollout_path: None,
                ..test_thread_session(side_thread_id, test_path_buf("/tmp/side"))
            }),
            turns: Vec::new(),
            events: Vec::new(),
            replay_entries: Vec::new(),
            input_state: None,
        };

        assert!(!app.should_refresh_snapshot_session(
            side_thread_id,
            /*is_replay_only*/ false,
            &snapshot
        ));
    }

    #[tokio::test]
    async fn side_thread_snapshot_skips_session_header_preamble() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        while app_event_rx.try_recv().is_ok() {}

        let parent_thread_id = ThreadId::new();
        let side_thread_id = ThreadId::new();
        app.primary_thread_id = Some(parent_thread_id);
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(parent_thread_id));

        let snapshot = ThreadEventSnapshot {
            session: Some(ThreadSessionState {
                forked_from_id: Some(parent_thread_id),
                ..test_thread_session(side_thread_id, test_path_buf("/tmp/side"))
            }),
            turns: Vec::new(),
            events: Vec::new(),
            replay_entries: Vec::new(),
            input_state: None,
        };

        app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);

        let mut rendered_cells = Vec::new();
        while let Ok(event) = app_event_rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                rendered_cells.push(lines_to_single_string(&cell.display_lines(/*width*/ 120)));
            }
        }
        assert_eq!(app.chat_widget.thread_id(), Some(side_thread_id));
        assert_eq!(rendered_cells, Vec::<String>::new());
        assert_eq!(
            app.chat_widget.active_cell_transcript_lines(/*width*/ 120),
            None
        );
    }

    #[tokio::test]
    async fn side_thread_ignores_global_mcp_startup_notifications() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        while app_event_rx.try_recv().is_ok() {}
        let app_server = crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
            .await
            .expect("embedded app server");
        let parent_thread_id = ThreadId::new();
        let side_thread_id = ThreadId::new();
        app.primary_thread_id = Some(parent_thread_id);
        app.active_thread_id = Some(side_thread_id);
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(parent_thread_id));
        app.sync_side_thread_ui();

        app.handle_app_server_event(
            &app_server,
            codex_app_server_client::AppServerEvent::ServerNotification(
                ServerNotification::McpServerStatusUpdated(McpServerStatusUpdatedNotification {
                    name: "sentry".to_string(),
                    status: McpServerStartupState::Failed,
                    error: Some("sentry is not logged in".to_string()),
                }),
            ),
        )
        .await;

        assert!(app_event_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn side_restore_user_message_puts_inline_question_back_in_composer() {
        let mut app = make_test_app().await;
        let user_message = crate::chatwidget::UserMessage::from("side question");

        app.restore_side_user_message(Some(user_message));

        assert_eq!(
            app.chat_widget.composer_text_with_pending(),
            "side question"
        );
    }

    #[tokio::test]
    async fn side_discard_selection_keeps_current_side_thread() {
        let mut app = make_test_app().await;
        let parent_thread_id = ThreadId::new();
        let side_thread_id = ThreadId::new();
        app.active_thread_id = Some(side_thread_id);
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(parent_thread_id));

        assert_eq!(
            app.side_thread_to_discard_after_switch(side_thread_id),
            None
        );
        assert_eq!(
            app.side_thread_to_discard_after_switch(parent_thread_id),
            Some(side_thread_id)
        );
    }

    #[tokio::test]
    async fn discard_side_thread_removes_agent_navigation_entry() -> Result<()> {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref()).await?;
        let mut side_config = app.chat_widget.config_ref().clone();
        side_config.ephemeral = true;
        let started = app_server.start_thread(&side_config).await?;
        let side_thread_id = started.session.thread_id;
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(ThreadId::new()));
        app.agent_navigation.upsert(
            side_thread_id,
            Some("Side".to_string()),
            Some("side".to_string()),
            /*is_closed*/ false,
        );

        assert!(
            app.discard_side_thread(&mut app_server, side_thread_id)
                .await
        );

        assert_eq!(app.agent_navigation.get(&side_thread_id), None);
        assert!(!app.side_threads.contains_key(&side_thread_id));
        Ok(())
    }

    #[tokio::test]
    async fn discard_side_thread_keeps_local_state_when_server_close_fails() -> Result<()> {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref()).await?;
        let parent_thread_id = ThreadId::new();
        let side_thread_id = ThreadId::new();
        app.active_thread_id = Some(side_thread_id);
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(parent_thread_id));
        app.agent_navigation.upsert(
            side_thread_id,
            Some("Side".to_string()),
            Some("side".to_string()),
            /*is_closed*/ false,
        );

        assert!(
            !app.discard_side_thread(&mut app_server, side_thread_id)
                .await
        );

        assert_eq!(app.active_thread_id, Some(side_thread_id));
        assert_eq!(
            app.side_threads
                .get(&side_thread_id)
                .map(|state| state.parent_thread_id),
            Some(parent_thread_id)
        );
        assert!(app.agent_navigation.get(&side_thread_id).is_some());
        Ok(())
    }

    #[tokio::test]
    async fn discard_closed_side_thread_removes_local_state_without_server_rpc() {
        let mut app = make_test_app().await;
        let parent_thread_id = ThreadId::new();
        let side_thread_id = ThreadId::new();
        app.active_thread_id = Some(side_thread_id);
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(parent_thread_id));
        app.thread_event_channels
            .insert(side_thread_id, ThreadEventChannel::new(/*capacity*/ 4));
        app.agent_navigation.upsert(
            side_thread_id,
            Some("Side".to_string()),
            Some("side".to_string()),
            /*is_closed*/ false,
        );

        app.discard_closed_side_thread(side_thread_id).await;

        assert_eq!(app.active_thread_id, None);
        assert!(!app.side_threads.contains_key(&side_thread_id));
        assert!(!app.thread_event_channels.contains_key(&side_thread_id));
        assert_eq!(app.agent_navigation.get(&side_thread_id), None);
    }

    #[tokio::test]
    async fn active_non_primary_shutdown_target_returns_none_for_non_shutdown_event() -> Result<()>
    {
        let mut app = make_test_app().await;
        app.active_thread_id = Some(ThreadId::new());
        app.primary_thread_id = Some(ThreadId::new());

        assert_eq!(
            app.active_non_primary_shutdown_target(&ServerNotification::SkillsChanged(
                codex_app_server_protocol::SkillsChangedNotification {},
            )),
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn active_non_primary_shutdown_target_returns_none_for_primary_thread_shutdown()
    -> Result<()> {
        let mut app = make_test_app().await;
        let thread_id = ThreadId::new();
        app.active_thread_id = Some(thread_id);
        app.primary_thread_id = Some(thread_id);

        assert_eq!(
            app.active_non_primary_shutdown_target(&thread_closed_notification(thread_id)),
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn active_non_primary_shutdown_target_returns_ids_for_non_primary_shutdown() -> Result<()>
    {
        let mut app = make_test_app().await;
        let active_thread_id = ThreadId::new();
        let primary_thread_id = ThreadId::new();
        app.active_thread_id = Some(active_thread_id);
        app.primary_thread_id = Some(primary_thread_id);

        assert_eq!(
            app.active_non_primary_shutdown_target(&thread_closed_notification(active_thread_id)),
            Some((active_thread_id, primary_thread_id))
        );
        Ok(())
    }

    #[tokio::test]
    async fn active_non_primary_shutdown_target_returns_none_when_shutdown_exit_is_pending()
    -> Result<()> {
        let mut app = make_test_app().await;
        let active_thread_id = ThreadId::new();
        let primary_thread_id = ThreadId::new();
        app.active_thread_id = Some(active_thread_id);
        app.primary_thread_id = Some(primary_thread_id);
        app.pending_shutdown_exit_thread_id = Some(active_thread_id);

        assert_eq!(
            app.active_non_primary_shutdown_target(&thread_closed_notification(active_thread_id)),
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn active_non_primary_shutdown_target_still_switches_for_other_pending_exit_thread()
    -> Result<()> {
        let mut app = make_test_app().await;
        let active_thread_id = ThreadId::new();
        let primary_thread_id = ThreadId::new();
        app.active_thread_id = Some(active_thread_id);
        app.primary_thread_id = Some(primary_thread_id);
        app.pending_shutdown_exit_thread_id = Some(ThreadId::new());

        assert_eq!(
            app.active_non_primary_shutdown_target(&thread_closed_notification(active_thread_id)),
            Some((active_thread_id, primary_thread_id))
        );
        Ok(())
    }

    async fn render_clear_ui_header_after_long_transcript_for_snapshot() -> String {
        let mut app = make_test_app().await;
        app.config.cwd = test_path_buf("/tmp/project").abs();
        app.chat_widget.set_model("gpt-test");
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::High));
        let story_part_one = "In the cliffside town of Bracken Ferry, the lighthouse had been dark for \
            nineteen years, and the children were told it was because the sea no longer wanted a \
            guide. Mara, who repaired clocks for a living, found that hard to believe. Every dawn she \
            heard the gulls circling the empty tower, and every dusk she watched ships hesitate at the \
            mouth of the bay as if listening for a signal that never came. When an old brass key fell \
            out of a cracked parcel in her workshop, tagged only with the words 'for the lamp room,' \
            she decided to climb the hill and see what the town had forgotten.";
        let story_part_two = "Inside the lighthouse she found gears wrapped in oilcloth, logbooks filled \
            with weather notes, and a lens shrouded beneath salt-stiff canvas. The mechanism was not \
            broken, only unfinished. Someone had removed the governor spring and hidden it in a false \
            drawer, along with a letter from the last keeper admitting he had darkened the light on \
            purpose after smugglers threatened his family. Mara spent the night rebuilding the clockwork \
            from spare watch parts, her fingers blackened with soot and grease, while a storm gathered \
            over the water and the harbor bells began to ring.";
        let story_part_three = "At midnight the first squall hit, and the fishing boats returned early, \
            blind in sheets of rain. Mara wound the mechanism, set the teeth by hand, and watched the \
            great lens begin to turn in slow, certain arcs. The beam swept across the bay, caught the \
            whitecaps, and reached the boats just as they were drifting toward the rocks below the \
            eastern cliffs. In the morning the town square was crowded with wet sailors, angry elders, \
            and wide-eyed children, but when the oldest captain placed the keeper's log on the fountain \
            and thanked Mara for relighting the coast, nobody argued. By sunset, Bracken Ferry had a \
            lighthouse again, and Mara had more clocks to mend than ever because everyone wanted \
            something in town to keep better time.";

        let user_cell = |text: &str| -> Arc<dyn HistoryCell> {
            Arc::new(UserHistoryCell {
                message: text.to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>
        };
        let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
            Arc::new(AgentMessageCell::new(
                vec![Line::from(text.to_string())],
                /*is_first_line*/ true,
            )) as Arc<dyn HistoryCell>
        };
        let make_header = |is_first| -> Arc<dyn HistoryCell> {
            let event = SessionConfiguredEvent {
                session_id: ThreadId::new(),
                forked_from_id: None,
                thread_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                permission_profile: Some(PermissionProfile::from_legacy_sandbox_policy(
                    &SandboxPolicy::new_read_only_policy(),
                )),
                cwd: test_path_buf("/tmp/project").abs(),
                reasoning_effort: Some(ReasoningEffortConfig::High),
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
                rollout_path: Some(PathBuf::new()),
            };
            Arc::new(new_session_info(
                app.chat_widget.config_ref(),
                app.chat_widget.current_model(),
                event,
                is_first,
                /*tooltip_override*/ None,
                /*auth_plan*/ None,
                /*show_fast_status*/ false,
            )) as Arc<dyn HistoryCell>
        };

        app.transcript_cells = vec![
            make_header(true),
            Arc::new(crate::history_cell::new_info_event(
                "startup tip that used to replay".to_string(),
                /*hint*/ None,
            )) as Arc<dyn HistoryCell>,
            user_cell("Tell me a long story about a town with a dark lighthouse."),
            agent_cell(story_part_one),
            user_cell("Continue the story and reveal why the light went out."),
            agent_cell(story_part_two),
            user_cell("Finish the story with a storm and a resolution."),
            agent_cell(story_part_three),
        ];
        app.has_emitted_history_lines = true;

        let rendered = app
            .clear_ui_header_lines_with_version(/*width*/ 80, "<VERSION>")
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !rendered.contains("startup tip that used to replay"),
            "clear header should not replay startup notices"
        );
        assert!(
            !rendered.contains("Bracken Ferry"),
            "clear header should not replay prior conversation turns"
        );
        rendered
    }

    #[tokio::test]
    #[cfg_attr(
        target_os = "windows",
        ignore = "snapshot path rendering differs on Windows"
    )]
    async fn clear_ui_after_long_transcript_snapshots_fresh_header_only() {
        let rendered = render_clear_ui_header_after_long_transcript_for_snapshot().await;
        assert_snapshot!("clear_ui_after_long_transcript_fresh_header_only", rendered);
    }

    #[tokio::test]
    #[cfg_attr(
        target_os = "windows",
        ignore = "snapshot path rendering differs on Windows"
    )]
    async fn ctrl_l_clear_ui_after_long_transcript_reuses_clear_header_snapshot() {
        let rendered = render_clear_ui_header_after_long_transcript_for_snapshot().await;
        assert_snapshot!("clear_ui_after_long_transcript_fresh_header_only", rendered);
    }

    #[tokio::test]
    #[cfg_attr(
        target_os = "windows",
        ignore = "snapshot path rendering differs on Windows"
    )]
    async fn clear_ui_header_shows_fast_status_for_fast_capable_models() {
        let mut app = make_test_app().await;
        app.config.cwd = test_path_buf("/tmp/project").abs();
        app.chat_widget.set_model("gpt-5.5");
        set_fast_mode_test_catalog(&mut app.chat_widget);
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::XHigh));
        app.chat_widget
            .set_service_tier(Some(codex_protocol::config_types::ServiceTier::Fast));
        set_chatgpt_auth(&mut app.chat_widget);
        set_fast_mode_test_catalog(&mut app.chat_widget);

        let rendered = app
            .clear_ui_header_lines_with_version(/*width*/ 80, "<VERSION>")
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert_snapshot!("clear_ui_header_fast_status_fast_capable_models", rendered);
    }

    fn parse_fenced_oracle_response(json: &str) -> crate::oracle_supervisor::OracleResponse {
        parse_oracle_response(&format!("```oracle_control\n{json}\n```"))
    }

    async fn make_test_app() -> App {
        let (chat_widget, app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
        let config = chat_widget.config_ref().clone();
        let file_search = FileSearchManager::new(config.cwd.to_path_buf(), app_event_tx.clone());
        let model = crate::legacy_core::test_support::get_model_offline(config.model.as_deref());
        let session_telemetry = test_session_telemetry(&config, model.as_str());

        App {
            model_catalog: chat_widget.model_catalog(),
            session_telemetry,
            app_event_tx,
            chat_widget,
            config,
            active_profile: None,
            cli_kv_overrides: Vec::new(),
            harness_overrides: ConfigOverrides::default(),
            runtime_approval_policy_override: None,
            runtime_sandbox_policy_override: None,
            file_search,
            transcript_cells: Vec::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            transcript_reflow: TranscriptReflowState::default(),
            initial_history_replay_buffer: None,
            enhanced_keys_supported: false,
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
            terminal_title_invalid_items_warned: Arc::new(AtomicBool::new(false)),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            feedback: codex_feedback::CodexFeedback::new(),
            feedback_audience: FeedbackAudience::External,
            environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
            remote_app_server_url: None,
            remote_app_server_auth_token: None,
            pending_update_action: None,
            pending_shutdown_exit_thread_id: None,
            windows_sandbox: WindowsSandboxState::default(),
            thread_event_channels: HashMap::new(),
            thread_event_listener_tasks: HashMap::new(),
            agent_navigation: AgentNavigationState::default(),
            side_threads: HashMap::new(),
            active_thread_id: None,
            active_thread_rx: None,
            primary_thread_id: None,
            last_subagent_backfill_attempt: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
            pending_app_server_requests: PendingAppServerRequests::default(),
            pending_startup_ui_action: None,
            oracle_state: OracleSupervisorState::default(),
            oracle_picker_show_info: false,
            oracle_picker_remote_threads: Vec::new(),
            oracle_picker_include_new_thread: false,
            oracle_picker_remote_list_request_id: 0,
            oracle_picker_remote_list_pending: false,
            oracle_picker_remote_list_tick: 0,
            oracle_picker_remote_list_notice: None,
            oracle_broker: None,
            oracle_test_broker_hooks: Arc::new(StdMutex::new(OracleTestBrokerHooks::default())),
            exit_after_turn: false,
            exit_after_turn_observed_assistant_output: false,
            exit_after_turn_thread_id: None,
            exit_after_turn_turn_id: None,
            pending_plugin_enabled_writes: HashMap::new(),
        }
    }

    async fn make_test_app_with_channels() -> (
        App,
        tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
        tokio::sync::mpsc::UnboundedReceiver<Op>,
    ) {
        let (chat_widget, app_event_tx, rx, op_rx) = make_chatwidget_manual_with_sender().await;
        let config = chat_widget.config_ref().clone();
        let file_search = FileSearchManager::new(config.cwd.to_path_buf(), app_event_tx.clone());
        let model = crate::legacy_core::test_support::get_model_offline(config.model.as_deref());
        let session_telemetry = test_session_telemetry(&config, model.as_str());

        (
            App {
                model_catalog: chat_widget.model_catalog(),
                session_telemetry,
                app_event_tx,
                chat_widget,
                config,
                active_profile: None,
                cli_kv_overrides: Vec::new(),
                harness_overrides: ConfigOverrides::default(),
                runtime_approval_policy_override: None,
                runtime_sandbox_policy_override: None,
                file_search,
                transcript_cells: Vec::new(),
                overlay: None,
                deferred_history_lines: Vec::new(),
                has_emitted_history_lines: false,
                transcript_reflow: TranscriptReflowState::default(),
                initial_history_replay_buffer: None,
                enhanced_keys_supported: false,
                commit_anim_running: Arc::new(AtomicBool::new(false)),
                status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
                terminal_title_invalid_items_warned: Arc::new(AtomicBool::new(false)),
                backtrack: BacktrackState::default(),
                backtrack_render_pending: false,
                feedback: codex_feedback::CodexFeedback::new(),
                feedback_audience: FeedbackAudience::External,
                environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
                remote_app_server_url: None,
                remote_app_server_auth_token: None,
                pending_update_action: None,
                pending_shutdown_exit_thread_id: None,
                windows_sandbox: WindowsSandboxState::default(),
                thread_event_channels: HashMap::new(),
                thread_event_listener_tasks: HashMap::new(),
                agent_navigation: AgentNavigationState::default(),
                side_threads: HashMap::new(),
                active_thread_id: None,
                active_thread_rx: None,
                primary_thread_id: None,
                last_subagent_backfill_attempt: None,
                primary_session_configured: None,
                pending_primary_events: VecDeque::new(),
                pending_app_server_requests: PendingAppServerRequests::default(),
                pending_startup_ui_action: None,
                oracle_state: OracleSupervisorState::default(),
                oracle_picker_show_info: false,
                oracle_picker_remote_threads: Vec::new(),
                oracle_picker_include_new_thread: false,
                oracle_picker_remote_list_request_id: 0,
                oracle_picker_remote_list_pending: false,
                oracle_picker_remote_list_tick: 0,
                oracle_picker_remote_list_notice: None,
                oracle_broker: None,
                oracle_test_broker_hooks: Arc::new(StdMutex::new(OracleTestBrokerHooks::default())),
                exit_after_turn: false,
                exit_after_turn_observed_assistant_output: false,
                exit_after_turn_thread_id: None,
                exit_after_turn_turn_id: None,
                pending_plugin_enabled_writes: HashMap::new(),
            },
            rx,
            op_rx,
        )
    }

    fn test_thread_session(thread_id: ThreadId, cwd: PathBuf) -> ThreadSessionState {
        ThreadSessionState {
            thread_id,
            forked_from_id: None,
            fork_parent_title: None,
            thread_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            permission_profile: Some(PermissionProfile::from_legacy_sandbox_policy(
                &SandboxPolicy::new_read_only_policy(),
            )),
            cwd: cwd.abs(),
            instruction_source_paths: Vec::new(),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            network_proxy: None,
            rollout_path: Some(PathBuf::new()),
        }
    }

    fn test_turn(turn_id: &str, status: TurnStatus, items: Vec<ThreadItem>) -> Turn {
        Turn {
            id: turn_id.to_string(),
            items,
            status,
            error: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        }
    }

    fn turn_started_notification(thread_id: ThreadId, turn_id: &str) -> ServerNotification {
        ServerNotification::TurnStarted(TurnStartedNotification {
            thread_id: thread_id.to_string(),
            turn: Turn {
                started_at: Some(0),
                ..test_turn(turn_id, TurnStatus::InProgress, Vec::new())
            },
        })
    }

    fn turn_completed_notification(
        thread_id: ThreadId,
        turn_id: &str,
        status: TurnStatus,
    ) -> ServerNotification {
        ServerNotification::TurnCompleted(TurnCompletedNotification {
            thread_id: thread_id.to_string(),
            turn: Turn {
                completed_at: Some(0),
                duration_ms: Some(1),
                ..test_turn(turn_id, status, Vec::new())
            },
        })
    }

    fn thread_status_changed_notification(
        thread_id: ThreadId,
        status: codex_app_server_protocol::ThreadStatus,
    ) -> ServerNotification {
        ServerNotification::ThreadStatusChanged(
            codex_app_server_protocol::ThreadStatusChangedNotification {
                thread_id: thread_id.to_string(),
                status,
            },
        )
    }

    fn thread_closed_notification(thread_id: ThreadId) -> ServerNotification {
        ServerNotification::ThreadClosed(ThreadClosedNotification {
            thread_id: thread_id.to_string(),
        })
    }

    fn token_usage_notification(
        thread_id: ThreadId,
        turn_id: &str,
        model_context_window: Option<i64>,
    ) -> ServerNotification {
        ServerNotification::ThreadTokenUsageUpdated(ThreadTokenUsageUpdatedNotification {
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            token_usage: ThreadTokenUsage {
                total: TokenUsageBreakdown {
                    total_tokens: 10,
                    input_tokens: 4,
                    cached_input_tokens: 1,
                    output_tokens: 5,
                    reasoning_output_tokens: 0,
                },
                last: TokenUsageBreakdown {
                    total_tokens: 10,
                    input_tokens: 4,
                    cached_input_tokens: 1,
                    output_tokens: 5,
                    reasoning_output_tokens: 0,
                },
                model_context_window,
            },
        })
    }

    fn hook_started_notification(thread_id: ThreadId, turn_id: &str) -> ServerNotification {
        ServerNotification::HookStarted(HookStartedNotification {
            thread_id: thread_id.to_string(),
            turn_id: Some(turn_id.to_string()),
            run: AppServerHookRunSummary {
                id: "user-prompt-submit:0:/tmp/hooks.json".to_string(),
                event_name: AppServerHookEventName::UserPromptSubmit,
                handler_type: AppServerHookHandlerType::Command,
                execution_mode: AppServerHookExecutionMode::Sync,
                scope: AppServerHookScope::Turn,
                source_path: test_path_buf("/tmp/hooks.json").abs(),
                source: codex_app_server_protocol::HookSource::User,
                display_order: 0,
                status: AppServerHookRunStatus::Running,
                status_message: Some("checking go-workflow input policy".to_string()),
                started_at: 1,
                completed_at: None,
                duration_ms: None,
                entries: Vec::new(),
            },
        })
    }

    fn hook_completed_notification(thread_id: ThreadId, turn_id: &str) -> ServerNotification {
        ServerNotification::HookCompleted(HookCompletedNotification {
            thread_id: thread_id.to_string(),
            turn_id: Some(turn_id.to_string()),
            run: AppServerHookRunSummary {
                id: "user-prompt-submit:0:/tmp/hooks.json".to_string(),
                event_name: AppServerHookEventName::UserPromptSubmit,
                handler_type: AppServerHookHandlerType::Command,
                execution_mode: AppServerHookExecutionMode::Sync,
                scope: AppServerHookScope::Turn,
                source_path: test_path_buf("/tmp/hooks.json").abs(),
                source: codex_app_server_protocol::HookSource::User,
                display_order: 0,
                status: AppServerHookRunStatus::Stopped,
                status_message: Some("checking go-workflow input policy".to_string()),
                started_at: 1,
                completed_at: Some(11),
                duration_ms: Some(10),
                entries: vec![
                    AppServerHookOutputEntry {
                        kind: AppServerHookOutputEntryKind::Warning,
                        text: "go-workflow must start from PlanMode".to_string(),
                    },
                    AppServerHookOutputEntry {
                        kind: AppServerHookOutputEntryKind::Stop,
                        text: "prompt blocked".to_string(),
                    },
                ],
            },
        })
    }

    fn agent_message_delta_notification(
        thread_id: ThreadId,
        turn_id: &str,
        item_id: &str,
        delta: &str,
    ) -> ServerNotification {
        ServerNotification::AgentMessageDelta(AgentMessageDeltaNotification {
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            item_id: item_id.to_string(),
            delta: delta.to_string(),
        })
    }

    fn exec_approval_request(
        thread_id: ThreadId,
        turn_id: &str,
        item_id: &str,
        approval_id: Option<&str>,
    ) -> ServerRequest {
        ServerRequest::CommandExecutionRequestApproval {
            request_id: AppServerRequestId::Integer(1),
            params: CommandExecutionRequestApprovalParams {
                thread_id: thread_id.to_string(),
                turn_id: turn_id.to_string(),
                item_id: item_id.to_string(),
                approval_id: approval_id.map(str::to_string),
                reason: Some("needs approval".to_string()),
                network_approval_context: None,
                command: Some("echo hello".to_string()),
                cwd: Some(test_path_buf("/tmp/project").abs()),
                command_actions: None,
                additional_permissions: None,
                proposed_execpolicy_amendment: None,
                proposed_network_policy_amendments: None,
                available_decisions: None,
            },
        }
    }

    fn request_user_input_request(
        thread_id: ThreadId,
        turn_id: &str,
        item_id: &str,
    ) -> ServerRequest {
        ServerRequest::ToolRequestUserInput {
            request_id: AppServerRequestId::Integer(2),
            params: ToolRequestUserInputParams {
                thread_id: thread_id.to_string(),
                turn_id: turn_id.to_string(),
                item_id: item_id.to_string(),
                questions: Vec::new(),
            },
        }
    }

    #[test]
    fn thread_event_store_tracks_active_turn_lifecycle() {
        let mut store = ThreadEventStore::new(/*capacity*/ 8);
        assert_eq!(store.active_turn_id(), None);

        let thread_id = ThreadId::new();
        store.push_notification(turn_started_notification(thread_id, "turn-1"));
        assert_eq!(store.active_turn_id(), Some("turn-1"));

        store.push_notification(turn_completed_notification(
            thread_id,
            "turn-2",
            TurnStatus::Completed,
        ));
        assert_eq!(store.active_turn_id(), Some("turn-1"));

        store.push_notification(turn_completed_notification(
            thread_id,
            "turn-1",
            TurnStatus::Interrupted,
        ));
        assert_eq!(store.active_turn_id(), None);
    }

    #[test]
    fn thread_event_store_restores_active_turn_from_snapshot_turns() {
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
        let turns = vec![
            test_turn("turn-1", TurnStatus::Completed, Vec::new()),
            test_turn("turn-2", TurnStatus::InProgress, Vec::new()),
        ];

        let store =
            ThreadEventStore::new_with_session(/*capacity*/ 8, session.clone(), turns.clone());
        assert_eq!(store.active_turn_id(), Some("turn-2"));

        let mut refreshed_store = ThreadEventStore::new(/*capacity*/ 8);
        refreshed_store.set_session(session, turns);
        assert_eq!(refreshed_store.active_turn_id(), Some("turn-2"));
    }

    #[test]
    fn thread_event_store_clear_active_turn_id_resets_cached_turn() {
        let mut store = ThreadEventStore::new(/*capacity*/ 8);
        let thread_id = ThreadId::new();
        store.push_notification(turn_started_notification(thread_id, "turn-1"));

        store.clear_active_turn_id();

        assert_eq!(store.active_turn_id(), None);
    }

    #[test]
    fn thread_event_store_rebase_preserves_resolved_request_state() {
        let thread_id = ThreadId::new();
        let mut store = ThreadEventStore::new(/*capacity*/ 8);
        store.push_request(exec_approval_request(
            thread_id,
            "turn-approval",
            "call-approval",
            /*approval_id*/ None,
        ));
        store.push_notification(ServerNotification::ServerRequestResolved(
            codex_app_server_protocol::ServerRequestResolvedNotification {
                request_id: AppServerRequestId::Integer(1),
                thread_id: thread_id.to_string(),
            },
        ));

        store.rebase_buffer_after_session_refresh();

        let snapshot = store.snapshot();
        assert!(snapshot.events.is_empty());
        assert_eq!(store.has_pending_thread_approvals(), false);
    }

    #[test]
    fn thread_event_store_rebase_preserves_hook_notifications() {
        let thread_id = ThreadId::new();
        let mut store = ThreadEventStore::new(/*capacity*/ 8);
        store.push_notification(hook_started_notification(thread_id, "turn-hook"));
        store.push_notification(hook_completed_notification(thread_id, "turn-hook"));

        store.rebase_buffer_after_session_refresh();

        let snapshot = store.snapshot();
        let hook_notifications = snapshot
            .events
            .into_iter()
            .map(|event| match event {
                ThreadBufferedEvent::Notification(notification) => {
                    serde_json::to_value(notification).expect("hook notification should serialize")
                }
                other => panic!("expected buffered hook notification, saw: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            hook_notifications,
            vec![
                serde_json::to_value(hook_started_notification(thread_id, "turn-hook"))
                    .expect("hook notification should serialize"),
                serde_json::to_value(hook_completed_notification(thread_id, "turn-hook"))
                    .expect("hook notification should serialize"),
            ]
        );
    }

    #[test]
    fn thread_event_store_refresh_preserves_oracle_workflow_order_for_completed_turns() {
        let thread_id = ThreadId::new();
        let session = test_thread_session(thread_id, PathBuf::from("/tmp/project"));
        let user_item = ThreadItem::UserMessage {
            id: "user-1".to_string(),
            content: vec![AppServerUserInput::Text {
                text: "Compute 2+2.".to_string(),
                text_elements: Vec::new(),
            }],
        };
        let reply_item = ThreadItem::AgentMessage {
            id: "assistant-1".to_string(),
            text: "The result is 4.".to_string(),
            phase: None,
            memory_citation: None,
        };
        let in_progress_turn = test_turn(
            "oracle-turn-1",
            TurnStatus::InProgress,
            vec![user_item.clone()],
        );
        let completed_turn = test_turn(
            "oracle-turn-1",
            TurnStatus::Completed,
            vec![user_item, reply_item],
        );

        let mut store = ThreadEventStore::new_with_session(
            /*capacity*/ 8,
            session.clone(),
            vec![in_progress_turn],
        );
        store.push_buffered_event(ThreadBufferedEvent::OracleWorkflowEvent(
            OracleWorkflowThreadEvent {
                title: "Oracle delegated work to the orchestrator.".to_string(),
                details: vec!["Workflow: oracle-wf-test v1".to_string()],
            },
        ));

        store.set_session(session, vec![completed_turn.clone()]);
        store.rebase_buffer_after_session_refresh();

        let replay_entries = store.snapshot().replay_entries;
        assert_eq!(replay_entries.len(), 3);
        assert_matches!(
            &replay_entries[0],
            ThreadReplayEntry::Turn(turn)
                if turn.id == "oracle-turn-1" && turn.status == TurnStatus::InProgress
        );
        assert_matches!(
            &replay_entries[1],
            ThreadReplayEntry::Event(event)
                if matches!(
                    event.as_ref(),
                    ThreadBufferedEvent::OracleWorkflowEvent(workflow_event)
                        if workflow_event.title == "Oracle delegated work to the orchestrator."
                )
        );
        assert_matches!(
            &replay_entries[2],
            ThreadReplayEntry::Turn(turn)
                if *turn == completed_turn
        );
    }

    #[test]
    fn build_feedback_upload_params_includes_thread_id_and_rollout_path() {
        let thread_id = ThreadId::new();
        let rollout_path = PathBuf::from("/tmp/rollout.jsonl");

        let params = build_feedback_upload_params(
            Some(thread_id),
            Some(rollout_path.clone()),
            FeedbackCategory::SafetyCheck,
            Some("needs follow-up".to_string()),
            Some("turn-123".to_string()),
            /*include_logs*/ true,
        );

        assert_eq!(params.classification, "safety_check");
        assert_eq!(params.reason, Some("needs follow-up".to_string()));
        assert_eq!(params.thread_id, Some(thread_id.to_string()));
        assert_eq!(
            params
                .tags
                .as_ref()
                .and_then(|tags: &BTreeMap<String, String>| tags.get("turn_id"))
                .map(String::as_str),
            Some("turn-123")
        );
        assert_eq!(params.include_logs, true);
        assert_eq!(params.extra_log_files, Some(vec![rollout_path]));
    }

    #[test]
    fn build_feedback_upload_params_omits_rollout_path_without_logs() {
        let params = build_feedback_upload_params(
            /*origin_thread_id*/ None,
            Some(PathBuf::from("/tmp/rollout.jsonl")),
            FeedbackCategory::GoodResult,
            /*reason*/ None,
            /*turn_id*/ None,
            /*include_logs*/ false,
        );

        assert_eq!(params.classification, "good_result");
        assert_eq!(params.reason, None);
        assert_eq!(params.thread_id, None);
        assert_eq!(params.tags, None);
        assert_eq!(params.include_logs, false);
        assert_eq!(params.extra_log_files, None);
    }

    #[tokio::test]
    async fn feedback_submission_without_thread_emits_error_history_cell() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;

        app.handle_feedback_submitted(
            /*origin_thread_id*/ None,
            FeedbackCategory::Bug,
            /*include_logs*/ true,
            Err("boom".to_string()),
        )
        .await;

        let cell = match app_event_rx.try_recv() {
            Ok(AppEvent::InsertHistoryCell(cell)) => cell,
            other => panic!("expected feedback error history cell, saw {other:?}"),
        };
        assert_eq!(
            lines_to_single_string(&cell.display_lines(/*width*/ 120)),
            "■ Failed to upload feedback: boom"
        );
    }

    #[tokio::test]
    async fn feedback_submission_for_inactive_thread_replays_into_origin_thread() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let origin_thread_id = ThreadId::new();
        let active_thread_id = ThreadId::new();
        let origin_session = test_thread_session(origin_thread_id, test_path_buf("/tmp/origin"));
        let active_session = test_thread_session(active_thread_id, test_path_buf("/tmp/active"));
        app.thread_event_channels.insert(
            origin_thread_id,
            ThreadEventChannel::new_with_session(
                THREAD_EVENT_CHANNEL_CAPACITY,
                origin_session.clone(),
                Vec::new(),
            ),
        );
        app.thread_event_channels.insert(
            active_thread_id,
            ThreadEventChannel::new_with_session(
                THREAD_EVENT_CHANNEL_CAPACITY,
                active_session.clone(),
                Vec::new(),
            ),
        );
        app.activate_thread_channel(active_thread_id).await;
        app.chat_widget.handle_thread_session(active_session);
        while app_event_rx.try_recv().is_ok() {}

        app.handle_feedback_submitted(
            Some(origin_thread_id),
            FeedbackCategory::Bug,
            /*include_logs*/ true,
            Ok("uploaded-thread".to_string()),
        )
        .await;

        assert_matches!(
            app_event_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        );

        let snapshot = {
            let channel = app
                .thread_event_channels
                .get(&origin_thread_id)
                .expect("origin thread channel should exist");
            let store = channel.store.lock().await;
            assert!(matches!(
                store.buffer.back(),
                Some(ThreadBufferedEvent::FeedbackSubmission(_))
            ));
            store.snapshot()
        };

        app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);

        let mut rendered_cells = Vec::new();
        while let Ok(event) = app_event_rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                rendered_cells.push(lines_to_single_string(&cell.display_lines(/*width*/ 120)));
            }
        }
        assert!(rendered_cells.iter().any(|cell| {
            cell.contains("• Feedback uploaded. Please open an issue using the following URL:")
                && cell.contains("uploaded-thread")
        }));
    }

    fn next_user_turn_op(op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) -> Op {
        let mut seen = Vec::new();
        while let Ok(op) = op_rx.try_recv() {
            if matches!(op, Op::UserTurn { .. }) {
                return op;
            }
            seen.push(format!("{op:?}"));
        }
        panic!("expected UserTurn op, saw: {seen:?}");
    }

    fn lines_to_single_string(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_bottom_popup(app: &App, width: u16) -> String {
        let height = app.chat_widget.desired_height(width);
        let area = ratatui::layout::Rect::new(0, 0, width, height);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        app.chat_widget.render(area, &mut buf);

        let mut lines: Vec<String> = (0..area.height)
            .map(|row| {
                let mut line = String::new();
                for col in 0..area.width {
                    let symbol = buf[(area.x + col, area.y + row)].symbol();
                    if symbol.is_empty() {
                        line.push(' ');
                    } else {
                        line.push_str(symbol);
                    }
                }
                line.trim_end().to_string()
            })
            .collect();

        while lines.first().is_some_and(|line| line.trim().is_empty()) {
            lines.remove(0);
        }
        while lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.pop();
        }

        lines.join("\n")
    }

    fn test_session_telemetry(config: &Config, model: &str) -> SessionTelemetry {
        let model_info =
            crate::legacy_core::test_support::construct_model_info_offline(model, config);
        SessionTelemetry::new(
            ThreadId::new(),
            model,
            model_info.slug.as_str(),
            /*account_id*/ None,
            /*account_email*/ None,
            /*auth_mode*/ None,
            "test_originator".to_string(),
            /*log_user_prompts*/ false,
            "test".to_string(),
            SessionSource::Cli,
        )
    }

    fn app_enabled_in_effective_config(config: &Config, app_id: &str) -> Option<bool> {
        config
            .config_layer_stack
            .effective_config()
            .as_table()
            .and_then(|table| table.get("apps"))
            .and_then(TomlValue::as_table)
            .and_then(|apps| apps.get(app_id))
            .and_then(TomlValue::as_table)
            .and_then(|app| app.get("enabled"))
            .and_then(TomlValue::as_bool)
    }

    #[test]
    fn active_turn_not_steerable_turn_error_extracts_structured_server_error() {
        let turn_error = AppServerTurnError {
            message: "cannot steer a review turn".to_string(),
            codex_error_info: Some(AppServerCodexErrorInfo::ActiveTurnNotSteerable {
                turn_kind: AppServerNonSteerableTurnKind::Review,
            }),
            additional_details: None,
        };
        let error = TypedRequestError::Server {
            method: "turn/steer".to_string(),
            source: JSONRPCErrorError {
                code: -32602,
                message: turn_error.message.clone(),
                data: Some(serde_json::to_value(&turn_error).expect("turn error should serialize")),
            },
        };

        assert_eq!(
            active_turn_not_steerable_turn_error(&error),
            Some(turn_error)
        );
    }

    #[test]
    fn active_turn_steer_race_detects_missing_active_turn() {
        let error = TypedRequestError::Server {
            method: "turn/steer".to_string(),
            source: JSONRPCErrorError {
                code: -32602,
                message: "no active turn to steer".to_string(),
                data: None,
            },
        };

        assert_eq!(
            active_turn_steer_race(&error),
            Some(ActiveTurnSteerRace::Missing)
        );
        assert_eq!(active_turn_not_steerable_turn_error(&error), None);
    }

    #[test]
    fn active_turn_steer_race_extracts_actual_turn_id_from_mismatch() {
        let error = TypedRequestError::Server {
            method: "turn/steer".to_string(),
            source: JSONRPCErrorError {
                code: -32602,
                message: "expected active turn id `turn-expected` but found `turn-actual`"
                    .to_string(),
                data: None,
            },
        };

        assert_eq!(
            active_turn_steer_race(&error),
            Some(ActiveTurnSteerRace::ExpectedTurnMismatch {
                actual_turn_id: "turn-actual".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn update_reasoning_effort_updates_collaboration_mode() {
        let mut app = make_test_app().await;
        app.chat_widget
            .set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

        app.on_update_reasoning_effort(Some(ReasoningEffortConfig::High));

        assert_eq!(
            app.chat_widget.current_reasoning_effort(),
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(
            app.config.model_reasoning_effort,
            Some(ReasoningEffortConfig::High)
        );
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_loads_latest_apps_state() -> Result<()> {
        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        let app_id = "unit_test_refresh_in_memory_config_connector".to_string();

        assert_eq!(app_enabled_in_effective_config(&app.config, &app_id), None);

        ConfigEditsBuilder::new(&app.config.codex_home)
            .with_edits([
                ConfigEdit::SetPath {
                    segments: vec!["apps".to_string(), app_id.clone(), "enabled".to_string()],
                    value: false.into(),
                },
                ConfigEdit::SetPath {
                    segments: vec![
                        "apps".to_string(),
                        app_id.clone(),
                        "disabled_reason".to_string(),
                    ],
                    value: "user".into(),
                },
            ])
            .apply()
            .await
            .expect("persist app toggle");

        assert_eq!(app_enabled_in_effective_config(&app.config, &app_id), None);

        app.refresh_in_memory_config_from_disk().await?;

        assert_eq!(
            app_enabled_in_effective_config(&app.config, &app_id),
            Some(false)
        );
        Ok(())
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_best_effort_keeps_current_config_on_error()
    -> Result<()> {
        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        std::fs::write(codex_home.path().join("config.toml"), "[broken")?;
        let original_config = app.config.clone();

        app.refresh_in_memory_config_from_disk_best_effort("starting a new thread")
            .await;

        assert_eq!(app.config, original_config);
        Ok(())
    }

    #[tokio::test]
    async fn refresh_in_memory_config_from_disk_uses_active_chat_widget_cwd() -> Result<()> {
        let mut app = make_test_app().await;
        let original_cwd = app.config.cwd.clone();
        let next_cwd_tmp = tempdir()?;
        let next_cwd = next_cwd_tmp.path().to_path_buf();

        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: ThreadId::new(),
                forked_from_id: None,
                thread_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                permission_profile: Some(PermissionProfile::from_legacy_sandbox_policy(
                    &SandboxPolicy::new_read_only_policy(),
                )),
                cwd: next_cwd.clone().abs(),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
                rollout_path: Some(PathBuf::new()),
            }),
        });

        assert_eq!(app.chat_widget.config_ref().cwd.to_path_buf(), next_cwd);
        assert_eq!(app.config.cwd, original_cwd);

        app.refresh_in_memory_config_from_disk().await?;

        assert_eq!(app.config.cwd, app.chat_widget.config_ref().cwd);
        Ok(())
    }

    #[tokio::test]
    async fn rebuild_config_for_resume_or_fallback_uses_current_config_on_same_cwd_error()
    -> Result<()> {
        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        std::fs::write(codex_home.path().join("config.toml"), "[broken")?;
        let current_config = app.config.clone();
        let current_cwd = current_config.cwd.clone();

        let resume_config = app
            .rebuild_config_for_resume_or_fallback(&current_cwd, current_cwd.to_path_buf())
            .await?;

        assert_eq!(resume_config, current_config);
        Ok(())
    }

    #[tokio::test]
    async fn rebuild_config_for_resume_or_fallback_errors_when_cwd_changes() -> Result<()> {
        let mut app = make_test_app().await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        std::fs::write(codex_home.path().join("config.toml"), "[broken")?;
        let current_cwd = app.config.cwd.clone();
        let next_cwd_tmp = tempdir()?;
        let next_cwd = next_cwd_tmp.path().to_path_buf();

        let result = app
            .rebuild_config_for_resume_or_fallback(&current_cwd, next_cwd)
            .await;

        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn sync_tui_theme_selection_updates_chat_widget_config_copy() {
        let mut app = make_test_app().await;

        app.sync_tui_theme_selection("dracula".to_string());

        assert_eq!(app.config.tui_theme.as_deref(), Some("dracula"));
        assert_eq!(
            app.chat_widget.config_ref().tui_theme.as_deref(),
            Some("dracula")
        );
    }

    #[tokio::test]
    async fn fresh_session_config_uses_current_service_tier() {
        let mut app = make_test_app().await;
        app.chat_widget
            .set_service_tier(Some(codex_protocol::config_types::ServiceTier::Fast));

        let config = app.fresh_session_config();

        assert_eq!(
            config.service_tier,
            Some(codex_protocol::config_types::ServiceTier::Fast)
        );
    }

    #[tokio::test]
    async fn backtrack_selection_with_duplicate_history_targets_unique_turn() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        let user_cell = |text: &str,
                         text_elements: Vec<TextElement>,
                         local_image_paths: Vec<PathBuf>,
                         remote_image_urls: Vec<String>|
         -> Arc<dyn HistoryCell> {
            Arc::new(UserHistoryCell {
                message: text.to_string(),
                text_elements,
                local_image_paths,
                remote_image_urls,
            }) as Arc<dyn HistoryCell>
        };
        let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
            Arc::new(AgentMessageCell::new(
                vec![Line::from(text.to_string())],
                /*is_first_line*/ true,
            )) as Arc<dyn HistoryCell>
        };

        let make_header = |is_first| {
            let event = SessionConfiguredEvent {
                session_id: ThreadId::new(),
                forked_from_id: None,
                thread_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                permission_profile: Some(PermissionProfile::from_legacy_sandbox_policy(
                    &SandboxPolicy::new_read_only_policy(),
                )),
                cwd: test_path_buf("/home/user/project").abs(),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
                rollout_path: Some(PathBuf::new()),
            };
            Arc::new(new_session_info(
                app.chat_widget.config_ref(),
                app.chat_widget.current_model(),
                event,
                is_first,
                /*tooltip_override*/ None,
                /*auth_plan*/ None,
                /*show_fast_status*/ false,
            )) as Arc<dyn HistoryCell>
        };

        let placeholder = "[Image #1]";
        let edited_text = format!("follow-up (edited) {placeholder}");
        let edited_range = edited_text.len().saturating_sub(placeholder.len())..edited_text.len();
        let edited_text_elements = vec![TextElement::new(
            edited_range.into(),
            /*placeholder*/ None,
        )];
        let edited_local_image_paths = vec![PathBuf::from("/tmp/fake-image.png")];

        // Simulate a transcript with duplicated history (e.g., from prior backtracks)
        // and an edited turn appended after a session header boundary.
        app.transcript_cells = vec![
            make_header(true),
            user_cell("first question", Vec::new(), Vec::new(), Vec::new()),
            agent_cell("answer first"),
            user_cell("follow-up", Vec::new(), Vec::new(), Vec::new()),
            agent_cell("answer follow-up"),
            make_header(false),
            user_cell("first question", Vec::new(), Vec::new(), Vec::new()),
            agent_cell("answer first"),
            user_cell(
                &edited_text,
                edited_text_elements.clone(),
                edited_local_image_paths.clone(),
                vec!["https://example.com/backtrack.png".to_string()],
            ),
            agent_cell("answer edited"),
        ];

        assert_eq!(user_count(&app.transcript_cells), 2);

        let base_id = ThreadId::new();
        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: base_id,
                forked_from_id: None,
                thread_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                permission_profile: Some(PermissionProfile::from_legacy_sandbox_policy(
                    &SandboxPolicy::new_read_only_policy(),
                )),
                cwd: test_path_buf("/home/user/project").abs(),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
                rollout_path: Some(PathBuf::new()),
            }),
        });

        app.backtrack.base_id = Some(base_id);
        app.backtrack.primed = true;
        app.backtrack.nth_user_message = user_count(&app.transcript_cells).saturating_sub(1);

        let selection = app
            .confirm_backtrack_from_main()
            .expect("backtrack selection");
        assert_eq!(selection.nth_user_message, 1);
        assert_eq!(selection.prefill, edited_text);
        assert_eq!(selection.text_elements, edited_text_elements);
        assert_eq!(selection.local_image_paths, edited_local_image_paths);
        assert_eq!(
            selection.remote_image_urls,
            vec!["https://example.com/backtrack.png".to_string()]
        );

        app.apply_backtrack_rollback(selection);
        assert_eq!(
            app.chat_widget.remote_image_urls(),
            vec!["https://example.com/backtrack.png".to_string()]
        );

        let mut rollback_turns = None;
        while let Ok(op) = op_rx.try_recv() {
            if let Op::ThreadRollback { num_turns } = op {
                rollback_turns = Some(num_turns);
            }
        }

        assert_eq!(rollback_turns, Some(1));
    }

    #[tokio::test]
    async fn backtrack_remote_image_only_selection_clears_existing_composer_draft() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        app.transcript_cells = vec![Arc::new(UserHistoryCell {
            message: "original".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }) as Arc<dyn HistoryCell>];
        app.chat_widget
            .set_composer_text("stale draft".to_string(), Vec::new(), Vec::new());

        let remote_image_url = "https://example.com/remote-only.png".to_string();
        app.apply_backtrack_rollback(BacktrackSelection {
            nth_user_message: 0,
            prefill: String::new(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: vec![remote_image_url.clone()],
        });

        assert_eq!(app.chat_widget.composer_text_with_pending(), "");
        assert_eq!(app.chat_widget.remote_image_urls(), vec![remote_image_url]);

        let mut rollback_turns = None;
        while let Ok(op) = op_rx.try_recv() {
            if let Op::ThreadRollback { num_turns } = op {
                rollback_turns = Some(num_turns);
            }
        }
        assert_eq!(rollback_turns, Some(1));
    }

    #[tokio::test]
    async fn backtrack_resubmit_preserves_data_image_urls_in_user_turn() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        let thread_id = ThreadId::new();
        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: thread_id,
                forked_from_id: None,
                thread_name: None,
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                permission_profile: Some(PermissionProfile::from_legacy_sandbox_policy(
                    &SandboxPolicy::new_read_only_policy(),
                )),
                cwd: test_path_buf("/home/user/project").abs(),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
                rollout_path: Some(PathBuf::new()),
            }),
        });

        let data_image_url = "data:image/png;base64,abc123".to_string();
        app.transcript_cells = vec![Arc::new(UserHistoryCell {
            message: "please inspect this".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: vec![data_image_url.clone()],
        }) as Arc<dyn HistoryCell>];

        app.apply_backtrack_rollback(BacktrackSelection {
            nth_user_message: 0,
            prefill: "please inspect this".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: vec![data_image_url.clone()],
        });

        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let mut saw_rollback = false;
        let mut submitted_items: Option<Vec<UserInput>> = None;
        while let Ok(op) = op_rx.try_recv() {
            match op {
                Op::ThreadRollback { .. } => saw_rollback = true,
                Op::UserTurn { items, .. } => submitted_items = Some(items),
                _ => {}
            }
        }

        assert!(saw_rollback);
        let items = submitted_items.expect("expected user turn after backtrack resubmit");
        assert!(items.iter().any(|item| {
            matches!(
                item,
                UserInput::Image { image_url } if image_url == &data_image_url
            )
        }));
    }

    #[tokio::test]
    async fn replay_thread_snapshot_replays_turn_history_in_order() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        app.replay_thread_snapshot(
            ThreadEventSnapshot {
                session: Some(test_thread_session(
                    thread_id,
                    test_path_buf("/home/user/project"),
                )),
                turns: vec![
                    Turn {
                        id: "turn-1".to_string(),
                        items: vec![ThreadItem::UserMessage {
                            id: "user-1".to_string(),
                            content: vec![AppServerUserInput::Text {
                                text: "first prompt".to_string(),
                                text_elements: Vec::new(),
                            }],
                        }],
                        status: TurnStatus::Completed,
                        error: None,
                        started_at: None,
                        completed_at: None,
                        duration_ms: None,
                    },
                    Turn {
                        id: "turn-2".to_string(),
                        items: vec![
                            ThreadItem::UserMessage {
                                id: "user-2".to_string(),
                                content: vec![AppServerUserInput::Text {
                                    text: "third prompt".to_string(),
                                    text_elements: Vec::new(),
                                }],
                            },
                            ThreadItem::AgentMessage {
                                id: "assistant-2".to_string(),
                                text: "done".to_string(),
                                phase: None,
                                memory_citation: None,
                            },
                        ],
                        status: TurnStatus::Completed,
                        error: None,
                        started_at: None,
                        completed_at: None,
                        duration_ms: None,
                    },
                ],
                events: Vec::new(),
                replay_entries: Vec::new(),
                input_state: None,
            },
            /*resume_restored_queue*/ false,
        );

        while let Ok(event) = app_event_rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                let cell: Arc<dyn HistoryCell> = cell.into();
                app.transcript_cells.push(cell);
            }
        }

        let user_messages: Vec<String> = app
            .transcript_cells
            .iter()
            .filter_map(|cell| {
                cell.as_any()
                    .downcast_ref::<UserHistoryCell>()
                    .map(|cell| cell.message.clone())
            })
            .collect();
        assert_eq!(
            user_messages,
            vec!["first prompt".to_string(), "third prompt".to_string()]
        );
    }

    #[tokio::test]
    async fn replace_chat_widget_reseeds_collab_agent_metadata_for_replay() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let receiver_thread_id =
            ThreadId::from_string("019cff70-2599-75e2-af72-b958ce5dc1cc").expect("valid thread");
        app.agent_navigation.upsert(
            receiver_thread_id,
            Some("Robie".to_string()),
            Some("explorer".to_string()),
            /*is_closed*/ false,
        );

        let replacement = ChatWidget::new_with_app_event(ChatWidgetInit {
            config: app.config.clone(),
            frame_requester: crate::tui::FrameRequester::test_dummy(),
            app_event_tx: app.app_event_tx.clone(),
            initial_user_message: None,
            enhanced_keys_supported: app.enhanced_keys_supported,
            has_chatgpt_account: app.chat_widget.has_chatgpt_account(),
            model_catalog: app.model_catalog.clone(),
            feedback: app.feedback.clone(),
            is_first_run: false,
            status_account_display: app.chat_widget.status_account_display().cloned(),
            initial_plan_type: app.chat_widget.current_plan_type(),
            model: Some(app.chat_widget.current_model().to_string()),
            startup_tooltip_override: None,
            status_line_invalid_items_warned: app.status_line_invalid_items_warned.clone(),
            terminal_title_invalid_items_warned: app.terminal_title_invalid_items_warned.clone(),
            session_telemetry: app.session_telemetry.clone(),
        });
        app.replace_chat_widget(replacement);

        app.replay_thread_snapshot(
            ThreadEventSnapshot {
                session: None,
                turns: Vec::new(),
                events: vec![ThreadBufferedEvent::Notification(
                    ServerNotification::ItemStarted(codex_app_server_protocol::ItemStartedNotification {
                        thread_id: "thread-1".to_string(),
                        turn_id: "turn-1".to_string(),
                        item: ThreadItem::CollabAgentToolCall {
                            id: "wait-1".to_string(),
                            tool: codex_app_server_protocol::CollabAgentTool::Wait,
                            status: codex_app_server_protocol::CollabAgentToolCallStatus::InProgress,
                            sender_thread_id: ThreadId::new().to_string(),
                            receiver_thread_ids: vec![receiver_thread_id.to_string()],
                            prompt: None,
                            model: None,
                            reasoning_effort: None,
                            agents_states: HashMap::new(),
                        },
                    }),
                )],
                replay_entries: Vec::new(),
                input_state: None,
            },
            /*resume_restored_queue*/ false,
        );

        let mut saw_named_wait = false;
        while let Ok(event) = app_event_rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                let transcript = lines_to_single_string(&cell.transcript_lines(/*width*/ 80));
                saw_named_wait |= transcript.contains("Robie [explorer]");
            }
        }

        assert!(
            saw_named_wait,
            "expected replayed wait item to keep agent name"
        );
    }

    #[tokio::test]
    async fn replace_chat_widget_preserves_fork_notice_emission_state() {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        let mut session = test_thread_session(thread_id, PathBuf::from("/tmp/project"));
        session.forked_from_id = Some(ThreadId::new());

        app.chat_widget.handle_thread_session(session.clone());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match app_event_rx.recv().await {
                    Some(AppEvent::InsertHistoryCell(cell))
                        if lines_to_single_string(&cell.display_lines(/*width*/ 120))
                            .contains("Thread forked from") =>
                    {
                        break;
                    }
                    Some(_) => continue,
                    None => panic!("app event channel closed before fork notice arrived"),
                }
            }
        })
        .await
        .expect("timed out waiting for initial fork notice");

        let replacement = ChatWidget::new_with_app_event(ChatWidgetInit {
            config: app.config.clone(),
            frame_requester: crate::tui::FrameRequester::test_dummy(),
            app_event_tx: app.app_event_tx.clone(),
            initial_user_message: None,
            enhanced_keys_supported: app.enhanced_keys_supported,
            has_chatgpt_account: app.chat_widget.has_chatgpt_account(),
            model_catalog: app.model_catalog.clone(),
            feedback: app.feedback.clone(),
            is_first_run: false,
            status_account_display: app.chat_widget.status_account_display().cloned(),
            initial_plan_type: app.chat_widget.current_plan_type(),
            model: Some(app.chat_widget.current_model().to_string()),
            startup_tooltip_override: None,
            status_line_invalid_items_warned: app.status_line_invalid_items_warned.clone(),
            terminal_title_invalid_items_warned: app.terminal_title_invalid_items_warned.clone(),
            session_telemetry: app.session_telemetry.clone(),
        });
        app.replace_chat_widget(replacement);

        app.chat_widget.handle_thread_session(session);
        tokio::task::yield_now().await;

        let repeated_fork_notices = std::iter::from_fn(|| app_event_rx.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::InsertHistoryCell(cell) => {
                    Some(lines_to_single_string(&cell.display_lines(/*width*/ 120)))
                }
                _ => None,
            })
            .filter(|cell| cell.contains("Thread forked from"))
            .count();

        assert_eq!(repeated_fork_notices, 0);
    }

    #[tokio::test]
    async fn refreshed_snapshot_session_persists_resumed_turns() {
        let mut app = make_test_app().await;
        let thread_id = ThreadId::new();
        let initial_session = test_thread_session(thread_id, test_path_buf("/tmp/original"));
        app.thread_event_channels.insert(
            thread_id,
            ThreadEventChannel::new_with_session(
                /*capacity*/ 4,
                initial_session.clone(),
                Vec::new(),
            ),
        );

        let resumed_turns = vec![test_turn(
            "turn-1",
            TurnStatus::Completed,
            vec![ThreadItem::UserMessage {
                id: "user-1".to_string(),
                content: vec![AppServerUserInput::Text {
                    text: "restored prompt".to_string(),
                    text_elements: Vec::new(),
                }],
            }],
        )];
        let resumed_session = ThreadSessionState {
            cwd: test_path_buf("/tmp/refreshed").abs(),
            ..initial_session.clone()
        };
        let mut snapshot = ThreadEventSnapshot {
            session: Some(initial_session),
            turns: Vec::new(),
            events: Vec::new(),
            replay_entries: Vec::new(),
            input_state: None,
        };

        app.apply_refreshed_snapshot_thread(
            thread_id,
            AppServerStartedThread {
                session: resumed_session.clone(),
                turns: resumed_turns.clone(),
            },
            &mut snapshot,
        )
        .await;

        assert_eq!(snapshot.session, Some(resumed_session.clone()));
        assert_eq!(snapshot.turns, resumed_turns);

        let store = app
            .thread_event_channels
            .get(&thread_id)
            .expect("thread channel")
            .store
            .lock()
            .await;
        let store_snapshot = store.snapshot();
        assert_eq!(store_snapshot.session, Some(resumed_session));
        assert_eq!(store_snapshot.turns, snapshot.turns);
    }

    #[tokio::test]
    async fn queued_rollback_syncs_overlay_and_clears_deferred_history() {
        let mut app = make_test_app().await;
        app.transcript_cells = vec![
            Arc::new(UserHistoryCell {
                message: "first".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(
                vec![Line::from("after first")],
                /*is_first_line*/ false,
            )) as Arc<dyn HistoryCell>,
            Arc::new(UserHistoryCell {
                message: "second".to_string(),
                text_elements: Vec::new(),
                local_image_paths: Vec::new(),
                remote_image_urls: Vec::new(),
            }) as Arc<dyn HistoryCell>,
            Arc::new(AgentMessageCell::new(
                vec![Line::from("after second")],
                /*is_first_line*/ false,
            )) as Arc<dyn HistoryCell>,
        ];
        app.overlay = Some(Overlay::new_transcript(app.transcript_cells.clone()));
        app.deferred_history_lines = vec![Line::from("stale buffered line")];
        app.backtrack.overlay_preview_active = true;
        app.backtrack.nth_user_message = 1;

        let changed = app.apply_non_pending_thread_rollback(/*num_turns*/ 1);

        assert!(changed);
        assert!(app.backtrack_render_pending);
        assert!(app.deferred_history_lines.is_empty());
        assert_eq!(app.backtrack.nth_user_message, 0);
        let user_messages: Vec<String> = app
            .transcript_cells
            .iter()
            .filter_map(|cell| {
                cell.as_any()
                    .downcast_ref::<UserHistoryCell>()
                    .map(|cell| cell.message.clone())
            })
            .collect();
        assert_eq!(user_messages, vec!["first".to_string()]);
        let overlay_cell_count = match app.overlay.as_ref() {
            Some(Overlay::Transcript(t)) => t.committed_cell_count(),
            _ => panic!("expected transcript overlay"),
        };
        assert_eq!(overlay_cell_count, app.transcript_cells.len());
    }

    #[tokio::test]
    async fn thread_rollback_response_discards_queued_active_thread_events() {
        let mut app = make_test_app().await;
        let thread_id = ThreadId::new();
        let (tx, rx) = mpsc::channel(8);
        app.active_thread_id = Some(thread_id);
        app.active_thread_rx = Some(rx);
        tx.send(ThreadBufferedEvent::Notification(
            ServerNotification::ConfigWarning(ConfigWarningNotification {
                summary: "stale warning".to_string(),
                details: None,
                path: None,
                range: None,
            }),
        ))
        .await
        .expect("event should queue");

        app.handle_thread_rollback_response(
            thread_id,
            /*num_turns*/ 1,
            &ThreadRollbackResponse {
                thread: Thread {
                    id: thread_id.to_string(),
                    forked_from_id: None,
                    preview: String::new(),
                    ephemeral: false,
                    model_provider: "openai".to_string(),
                    created_at: 0,
                    updated_at: 0,
                    status: codex_app_server_protocol::ThreadStatus::Idle,
                    path: None,
                    cwd: test_path_buf("/tmp/project").abs(),
                    cli_version: "0.0.0".to_string(),
                    source: SessionSource::Cli.into(),
                    agent_nickname: None,
                    agent_role: None,
                    git_info: None,
                    name: None,
                    turns: Vec::new(),
                },
            },
        )
        .await;

        let rx = app
            .active_thread_rx
            .as_mut()
            .expect("active receiver should remain attached");
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn new_session_requests_shutdown_for_previous_conversation() {
        let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;

        let thread_id = ThreadId::new();
        let event = SessionConfiguredEvent {
            session_id: thread_id,
            forked_from_id: None,
            thread_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            sandbox_policy: SandboxPolicy::new_read_only_policy(),
            permission_profile: Some(PermissionProfile::from_legacy_sandbox_policy(
                &SandboxPolicy::new_read_only_policy(),
            )),
            cwd: test_path_buf("/home/user/project").abs(),
            reasoning_effort: None,
            history_log_id: 0,
            history_entry_count: 0,
            initial_messages: None,
            network_proxy: None,
            rollout_path: Some(PathBuf::new()),
        };

        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(event),
        });

        while app_event_rx.try_recv().is_ok() {}
        while op_rx.try_recv().is_ok() {}

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        app.shutdown_current_thread(&mut app_server).await;

        assert!(
            op_rx.try_recv().is_err(),
            "shutdown should not submit Op::Shutdown"
        );
    }

    #[tokio::test]
    async fn shutdown_first_exit_returns_immediate_exit_when_shutdown_submit_fails() {
        let mut app = make_test_app().await;
        let thread_id = ThreadId::new();
        app.active_thread_id = Some(thread_id);

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let control = app
            .handle_exit_mode(&mut app_server, ExitMode::ShutdownFirst)
            .await;

        assert_eq!(app.pending_shutdown_exit_thread_id, None);
        assert!(matches!(
            control,
            AppRunControl::Exit(ExitReason::UserRequested)
        ));
    }

    #[tokio::test]
    async fn shutdown_first_exit_uses_app_server_shutdown_without_submitting_op() {
        let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
        let thread_id = ThreadId::new();
        app.active_thread_id = Some(thread_id);

        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let control = app
            .handle_exit_mode(&mut app_server, ExitMode::ShutdownFirst)
            .await;

        assert_eq!(app.pending_shutdown_exit_thread_id, None);
        assert!(matches!(
            control,
            AppRunControl::Exit(ExitReason::UserRequested)
        ));
        assert!(
            op_rx.try_recv().is_err(),
            "shutdown should not submit Op::Shutdown"
        );
    }

    #[tokio::test]
    async fn interrupt_without_active_turn_is_treated_as_handled() {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let started = app_server
            .start_thread(app.chat_widget.config_ref())
            .await
            .expect("thread/start should succeed");
        let thread_id = started.session.thread_id;
        app.enqueue_primary_thread_session(started.session, started.turns)
            .await
            .expect("primary thread should be registered");
        let op = AppCommand::interrupt();

        let handled = app
            .try_submit_active_thread_op_via_app_server(&mut app_server, thread_id, &op)
            .await
            .expect("interrupt submission should not fail");

        assert_eq!(handled, true);
    }

    #[tokio::test]
    async fn clear_only_ui_reset_preserves_chat_session_state() {
        let mut app = make_test_app().await;
        let thread_id = ThreadId::new();
        app.chat_widget.handle_codex_event(Event {
            id: String::new(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: thread_id,
                forked_from_id: None,
                thread_name: Some("keep me".to_string()),
                model: "gpt-test".to_string(),
                model_provider_id: "test-provider".to_string(),
                service_tier: None,
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                sandbox_policy: SandboxPolicy::new_read_only_policy(),
                permission_profile: Some(PermissionProfile::from_legacy_sandbox_policy(
                    &SandboxPolicy::new_read_only_policy(),
                )),
                cwd: test_path_buf("/tmp/project").abs(),
                reasoning_effort: None,
                history_log_id: 0,
                history_entry_count: 0,
                initial_messages: None,
                network_proxy: None,
                rollout_path: Some(PathBuf::new()),
            }),
        });
        app.chat_widget
            .apply_external_edit("draft prompt".to_string());
        app.transcript_cells = vec![Arc::new(UserHistoryCell {
            message: "old message".to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }) as Arc<dyn HistoryCell>];
        app.overlay = Some(Overlay::new_transcript(app.transcript_cells.clone()));
        app.deferred_history_lines = vec![Line::from("stale buffered line")];
        app.has_emitted_history_lines = true;
        app.backtrack.primed = true;
        app.backtrack.overlay_preview_active = true;
        app.backtrack.nth_user_message = 0;
        app.backtrack_render_pending = true;

        app.reset_app_ui_state_after_clear();

        assert!(app.overlay.is_none());
        assert!(app.transcript_cells.is_empty());
        assert!(app.deferred_history_lines.is_empty());
        assert!(!app.has_emitted_history_lines);
        assert!(!app.backtrack.primed);
        assert!(!app.backtrack.overlay_preview_active);
        assert!(app.backtrack.pending_rollback.is_none());
        assert!(!app.backtrack_render_pending);
        assert_eq!(app.chat_widget.thread_id(), Some(thread_id));
        assert_eq!(app.chat_widget.composer_text_with_pending(), "draft prompt");
    }

    #[tokio::test]
    async fn session_summary_skips_when_no_usage_or_resume_hint() {
        assert!(
            session_summary(
                TokenUsage::default(),
                /*thread_id*/ None,
                /*thread_name*/ None,
                /*rollout_path*/ None,
            )
            .is_none()
        );
    }

    #[tokio::test]
    async fn session_summary_skips_resume_hint_until_rollout_exists() {
        let usage = TokenUsage::default();
        let conversation = ThreadId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();
        let temp_dir = tempdir().expect("temp dir");
        let rollout_path = temp_dir.path().join("rollout.jsonl");

        assert!(
            session_summary(
                usage,
                Some(conversation),
                /*thread_name*/ None,
                Some(&rollout_path),
            )
            .is_none()
        );
    }

    #[tokio::test]
    async fn session_summary_includes_resume_hint_for_persisted_rollout() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Default::default()
        };
        let conversation = ThreadId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();
        let temp_dir = tempdir().expect("temp dir");
        let rollout_path = temp_dir.path().join("rollout.jsonl");
        std::fs::write(&rollout_path, "{}\n").expect("write rollout");

        let summary = session_summary(
            usage,
            Some(conversation),
            /*thread_name*/ None,
            Some(&rollout_path),
        )
        .expect("summary");
        assert_eq!(
            summary.usage_line,
            Some("Token usage: total=12 input=10 output=2".to_string())
        );
        assert_eq!(
            summary.resume_command,
            Some("codex resume 123e4567-e89b-12d3-a456-426614174000".to_string())
        );
    }

    #[tokio::test]
    async fn session_summary_uses_id_even_when_thread_has_name() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Default::default()
        };
        let conversation = ThreadId::from_string("123e4567-e89b-12d3-a456-426614174000").unwrap();
        let temp_dir = tempdir().expect("temp dir");
        let rollout_path = temp_dir.path().join("rollout.jsonl");
        std::fs::write(&rollout_path, "{}\n").expect("write rollout");

        let summary = session_summary(
            usage,
            Some(conversation),
            Some("my-session".to_string()),
            Some(&rollout_path),
        )
        .expect("summary");
        assert_eq!(
            summary.resume_command,
            Some("codex resume 123e4567-e89b-12d3-a456-426614174000".to_string())
        );
    }
}
