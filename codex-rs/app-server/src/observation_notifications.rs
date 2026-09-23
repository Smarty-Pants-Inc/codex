use codex_app_server_protocol::ObservationCaptureState;
use codex_app_server_protocol::ObservationSubmissionOutcome;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadObservationCapturedNotification;
use codex_app_server_protocol::ThreadObservationSubmittedNotification;
use codex_core::ObservationEvent;
use codex_core::ObservationOutcome;
use codex_core::ObservationOwner;
use codex_core::ObservationStatus;
use codex_protocol::ThreadId;

pub(crate) fn notification(
    thread_id: ThreadId,
    owner: ObservationOwner,
    event: ObservationEvent,
) -> Option<ServerNotification> {
    match event {
        ObservationEvent::Budget {
            owner: actual,
            commit_order,
            reservation: snapshot,
        } => {
            if actual != owner {
                return None;
            }
            Some(ServerNotification::ThreadObservationBudget(
                codex_app_server_protocol::ThreadObservationBudgetNotification {
                    protocol: 2,
                    thread_id: thread_id.to_string(),
                    owner_epoch: owner.epoch.to_string(),
                    commit_order,
                    native_reservation: reservation(snapshot),
                },
            ))
        }
        ObservationEvent::Captured(capture) => {
            if capture.metadata.owner != owner {
                return None;
            }
            Some(ServerNotification::ThreadObservationCaptured(
                ThreadObservationCapturedNotification {
                    protocol: 2,
                    budget_generation: capture.metadata.native_reservation.as_ref()?.generation,
                    thread_id: thread_id.to_string(),
                    turn_id: capture.turn_id,
                    owner_epoch: owner.epoch.to_string(),
                    decision_id: capture.decision_id.to_string(),
                    commit_order: capture.metadata.commit_order,
                    frame_revision: capture.metadata.revision,
                    frame_hash: capture.metadata.hash,
                    state: capture_state(capture.metadata.status),
                    captured_at: capture.captured_at,
                },
            ))
        }
        ObservationEvent::Submitted(record) => {
            if record.metadata.owner != owner {
                return None;
            }
            Some(ServerNotification::ThreadObservationSubmitted(
                ThreadObservationSubmittedNotification {
                    protocol: 2,
                    budget_generation: record.metadata.native_reservation.as_ref()?.generation,
                    thread_id: thread_id.to_string(),
                    turn_id: record.turn_id,
                    owner_epoch: owner.epoch.to_string(),
                    decision_id: record.decision_id.to_string(),
                    attempt_id: record.attempt_id.to_string(),
                    request_id: record.request_id.to_string(),
                    provider_request_id: record.provider_request_id,
                    // Submission joins its immutable capture, not the internal
                    // audit journal's later terminal/retry order.
                    commit_order: record.metadata.commit_order,
                    frame_revision: record.metadata.revision,
                    frame_hash: record.metadata.hash,
                    state: capture_state(record.metadata.status),
                    captured_at: record.captured_at,
                    outcome: match record.outcome {
                        ObservationOutcome::Accepted => ObservationSubmissionOutcome::Accepted,
                        ObservationOutcome::Rejected => ObservationSubmissionOutcome::Rejected,
                        ObservationOutcome::Unknown => ObservationSubmissionOutcome::Unknown,
                    },
                    terminal_decision: record.terminal_decision,
                },
            ))
        }
        ObservationEvent::Published(_)
        | ObservationEvent::Read(_)
        | ObservationEvent::Control { .. } => None,
    }
}

pub(crate) fn reservation(
    snapshot: codex_core::ObservationReservation,
) -> codex_app_server_protocol::NativeReservation {
    use codex_app_server_protocol::NativeReservationProfile;
    use codex_app_server_protocol::NativeReservationState;
    codex_app_server_protocol::NativeReservation {
        generation: snapshot.generation,
        state: match snapshot.state {
            codex_core::ObservationReservationState::Valid => NativeReservationState::Valid,
            codex_core::ObservationReservationState::Invalid => NativeReservationState::Invalid,
            codex_core::ObservationReservationState::Unsupported => {
                NativeReservationState::Unsupported
            }
        },
        model: snapshot.model,
        profile: match snapshot.profile {
            codex_core::ObservationProfile::HarmonyGptOss => {
                NativeReservationProfile::HarmonyGptOss
            }
        },
        usable_context_tokens: snapshot.usable_context_tokens,
        reserved_tokens: snapshot.reserved_tokens,
        max_frame_bytes: snapshot.max_frame_bytes,
    }
}

fn capture_state(status: ObservationStatus) -> ObservationCaptureState {
    match status {
        ObservationStatus::Current => ObservationCaptureState::Current,
        ObservationStatus::Cleared => ObservationCaptureState::Cleared,
        ObservationStatus::Expired => ObservationCaptureState::Expired,
        ObservationStatus::Unavailable => ObservationCaptureState::Unavailable,
    }
}
