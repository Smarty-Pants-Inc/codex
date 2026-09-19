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
        ObservationEvent::Captured(capture) => {
            if capture.metadata.owner != owner {
                return None;
            }
            Some(ServerNotification::ThreadObservationCaptured(
                ThreadObservationCapturedNotification {
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
                    thread_id: thread_id.to_string(),
                    turn_id: record.turn_id,
                    owner_epoch: owner.epoch.to_string(),
                    decision_id: record.decision_id.to_string(),
                    attempt_id: record.attempt_id.to_string(),
                    request_id: record.request_id.to_string(),
                    provider_request_id: record.provider_request_id,
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
        ObservationEvent::Budget { .. }
        | ObservationEvent::Published(_)
        | ObservationEvent::Read(_)
        | ObservationEvent::Control { .. } => None,
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
