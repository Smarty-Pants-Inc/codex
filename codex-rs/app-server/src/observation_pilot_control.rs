//! Controls retain the original thread/slot, including one uncancellable retirement join.
use crate::observation_control::rejected;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::PilotNativeAttempt;
use codex_app_server_protocol::PilotNativeIdentity;
use codex_app_server_protocol::PilotNativeUsage;
use codex_app_server_protocol::PilotSourceOperation;
use codex_app_server_protocol::ThreadObservationRejectionCode;
use codex_app_server_protocol::ThreadPilotCheckResponse;
use codex_app_server_protocol::ThreadPilotReadResponse;
use codex_app_server_protocol::ThreadPilotRetireResponse;
use codex_app_server_protocol::ThreadPilotStartResponse;
use codex_core::CodexThread;
use codex_core::ObservationOwner;
use codex_core::ObservationSlot;
use codex_core::PilotPermission;
use codex_core::PilotReport;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::turn_input::StartIfIdleSubmission;
use codex_protocol::turn_input::TurnInput;
use codex_protocol::turn_input::TurnInputRequest;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::watch;
use uuid::Uuid;

#[cfg(test)]
#[path = "observation_pilot_control_tests.rs"]
mod tests;

type Retirement = Result<ThreadPilotRetireResponse, JSONRPCErrorError>;
struct RetirementJoin {
    result: watch::Receiver<Option<Retirement>>,
    _task: tokio::task::JoinHandle<()>,
}

pub(crate) struct PilotControl {
    pub(crate) binding: ThreadPilotReadResponse,
    thread: Arc<CodexThread>,
    slot: Arc<ObservationSlot>,
    owner: ObservationOwner,
    scope: String,
    retirement: OnceLock<Result<RetirementJoin, JSONRPCErrorError>>,
}

fn denied() -> JSONRPCErrorError {
    rejected(ThreadObservationRejectionCode::Denied)
}

impl PilotControl {
    pub(crate) fn new(
        thread: Arc<CodexThread>,
        slot: Arc<ObservationSlot>,
        owner: ObservationOwner,
        scope: String,
        instruction: String,
        allocation_id: String,
        decision_sha256: String,
    ) -> Result<Arc<Self>, JSONRPCErrorError> {
        let report = slot.pilot_report(owner).map_err(|_| denied())?;
        Ok(Arc::new(Self {
            binding: ThreadPilotReadResponse {
                identity: identity(&report),
                instruction,
                allocation_id,
                decision_sha256,
            },
            thread,
            slot,
            owner,
            scope,
            retirement: OnceLock::new(),
        }))
    }

    pub(crate) fn check(
        &self,
        operation: PilotSourceOperation,
    ) -> Result<ThreadPilotCheckResponse, JSONRPCErrorError> {
        let permission = match operation {
            PilotSourceOperation::PrepareSource => PilotPermission::PrepareSource,
            PilotSourceOperation::SampleSource => PilotPermission::SampleSource,
            PilotSourceOperation::ActOnSource => PilotPermission::ActOnSource,
        };
        let request_id = Uuid::now_v7();
        self.thread
            .check_pilot_grant(self.owner, &self.scope, permission, request_id)
            .map_err(|_| denied())?;
        Ok(ThreadPilotCheckResponse {
            request_id: request_id.to_string(),
        })
    }

    pub(crate) async fn start(
        &self,
        input: String,
    ) -> Result<ThreadPilotStartResponse, JSONRPCErrorError> {
        if input.is_empty() || input.len() > 1024 {
            return Err(denied());
        }
        let request = TurnInputRequest::new(TurnInput::ResponseItem(ResponseItem::Message {
            id: None,
            role: "user".to_owned(),
            content: vec![ContentItem::InputText { text: input }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }));
        // Native idle reservation + issuer guard commits the ACTUAL turn identity.
        // Never implement D as sampled idle followed by ordinary foreground input.
        let result = self
            .thread
            .start_pilot_turn(self.owner, self.scope.clone(), Uuid::now_v7(), request)
            .await
            .map_err(|_| denied())?;
        Ok(match result {
            StartIfIdleSubmission::Started { turn_id } => ThreadPilotStartResponse {
                turn_id: Some(turn_id),
                started: true,
            },
            StartIfIdleSubmission::NotSubmitted { .. } => ThreadPilotStartResponse {
                turn_id: None,
                started: false,
            },
        })
    }

    pub(crate) fn begin_retirement(self: &Arc<Self>) -> Result<(), JSONRPCErrorError> {
        self.retirement_join()
            .as_ref()
            .map(|_| ())
            .map_err(Clone::clone)
    }

    fn retirement_join(self: &Arc<Self>) -> &Result<RetirementJoin, JSONRPCErrorError> {
        // Initialization has no await: fence the slot/issuer BEFORE spawning or
        // awaiting shutdown. Keep the one task and result after caller cancellation.
        self.retirement.get_or_init(|| {
            self.slot.pilot_report(self.owner).map_err(|_| denied())?;
            self.slot.revoke().map_err(|_| denied())?;
            let runtime = tokio::runtime::Handle::try_current().map_err(|_| denied())?;
            let (sender, result) = watch::channel(None);
            let original = Arc::clone(self);
            let task = runtime.spawn(async move {
                let report = original
                    .thread
                    .retire_pilot(original.owner)
                    .await
                    .map(|retired| wire_report(retired.report))
                    .map_err(|_| denied());
                sender.send_replace(Some(report));
            });
            Ok(RetirementJoin {
                result,
                _task: task,
            })
        })
    }

    pub(crate) async fn retire(self: &Arc<Self>) -> Retirement {
        let mut result = match self.retirement_join() {
            Ok(join) => join.result.clone(),
            Err(error) => return Err(error.clone()),
        };
        let completed = result
            .wait_for(Option::is_some)
            .await
            .map_err(|_| denied())?;
        completed.as_ref().cloned().ok_or_else(denied)?
    }
}

fn identity(report: &PilotReport) -> PilotNativeIdentity {
    PilotNativeIdentity {
        connection_id: report.owner.connection_id.to_string(),
        owner_epoch: report.owner.epoch.to_string(),
        thread_id: report.thread_id.to_string(),
        grant_id: report.grant_id.to_string(),
    }
}

fn wire_report(report: PilotReport) -> ThreadPilotRetireResponse {
    ThreadPilotRetireResponse {
        identity: identity(&report),
        revoked: report.revoked,
        active_decision: report.active_decision.map(|id| id.to_string()),
        admitted_turns: report
            .admitted_turns
            .into_iter()
            .map(|(turn, id)| (turn, id.to_string()))
            .collect(),
        reserved_tokens: report.reserved_tokens,
        attempts: report
            .attempts
            .into_iter()
            .map(|attempt| PilotNativeAttempt {
                decision_id: attempt.decision_id.to_string(),
                attempt_id: attempt.attempt_id.to_string(),
                request_id: attempt.request_id.to_string(),
                turn_id: attempt.turn_id,
                admission_request_id: attempt.admission_request_id.map(|id| id.to_string()),
                token_ceiling: attempt.reservation.token_ceiling.get(),
                credential_receipt: attempt.reservation.credential_receipt.to_string(),
                context_receipt: attempt.reservation.context_receipt.to_string(),
                response_id: attempt.response_id,
                usage: attempt.usage.map(|usage| PilotNativeUsage {
                    input_tokens: usage.input_tokens,
                    cached_input_tokens: usage.cached_input_tokens,
                    cache_write_input_tokens: usage.cache_write_input_tokens,
                    output_tokens: usage.output_tokens,
                    reasoning_output_tokens: usage.reasoning_output_tokens,
                    total_tokens: usage.total_tokens,
                }),
                response_complete: attempt.response_complete,
                usage_conflict: attempt.usage_conflict,
            })
            .collect(),
    }
}
