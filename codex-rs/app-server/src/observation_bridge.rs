//! One owner-bound FIFO consumer. Admission must install this in ThreadState
//! and start its relay before returning observation capabilities.
use crate::observation_control::rejected;
use crate::observation_control::store_error;
use crate::observation_notifications::notification;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::ConnectionRequestId;
use crate::outgoing_message::OutgoingMessageSender;
use crate::transport::ConnectionOrigin;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ObservationPublicationState;
use codex_app_server_protocol::ThreadObservationRejectionCode as Code;
use codex_app_server_protocol::ThreadObservationSetResponse;
use codex_core::ObservationEvent;
use codex_core::ObservationFrame;
use codex_core::ObservationOwner;
use codex_core::ObservationSlot;
use codex_core::ObservationStatus;
use codex_protocol::ThreadId;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(crate) enum ControlOperation {
    Set {
        revision: u64,
        frame: Option<ObservationFrame>,
    },
    Read,
}

enum ReplyKind {
    Set,
    Read,
}

struct Pending {
    request: ConnectionRequestId,
    kind: ReplyKind,
}

pub(crate) struct ObservationBridge {
    pub(crate) slot: Arc<ObservationSlot>,
    pub(crate) owner: ObservationOwner,
    pub(crate) thread_id: ThreadId,
    pub(crate) pilot: std::sync::OnceLock<Arc<crate::observation_pilot_control::PilotControl>>,
    pending: Mutex<HashMap<Uuid, Pending>>,
    cancelled: CancellationToken,
}

impl ObservationBridge {
    pub(crate) fn new(
        origin: ConnectionOrigin,
        connection_id: ConnectionId,
        thread_id: ThreadId,
    ) -> Result<(Arc<Self>, mpsc::Receiver<ObservationEvent>), JSONRPCErrorError> {
        if origin != ConnectionOrigin::Stdio {
            return Err(rejected(Code::Denied));
        }
        let (slot, events, owner) = ObservationSlot::new(connection_id.0);
        Ok((
            Arc::new(Self {
                slot: Arc::new(slot),
                owner,
                thread_id,
                pilot: std::sync::OnceLock::new(),
                pending: Mutex::new(HashMap::new()),
                cancelled: CancellationToken::new(),
            }),
            events,
        ))
    }

    /// No await/cancellation point between correlation reservation and commit.
    /// On success only the FIFO relay may answer this method.
    pub(crate) fn submit(
        &self,
        request: ConnectionRequestId,
        epoch: &str,
        operation: ControlOperation,
    ) -> Result<(), JSONRPCErrorError> {
        if request.connection_id.0 != self.owner.connection_id {
            return Err(rejected(Code::Denied));
        }
        if self.cancelled.is_cancelled() || Uuid::parse_str(epoch).ok() != Some(self.owner.epoch) {
            return Err(rejected(Code::StaleOwner));
        }
        let mut pending = self
            .pending
            .try_lock()
            .map_err(|_| rejected(Code::ResourceLimit))?;
        if pending.len() >= 32 || pending.values().any(|entry| entry.request == request) {
            return Err(rejected(Code::ResourceLimit));
        }
        let id = Uuid::new_v4();
        // Only metadata/correlation remain pending: never retain the frame body.
        let result = match operation {
            ControlOperation::Set { revision, frame } => {
                pending.insert(
                    id,
                    Pending {
                        request,
                        kind: ReplyKind::Set,
                    },
                );
                self.slot.set_for_request(self.owner, revision, frame, id)
            }
            ControlOperation::Read => {
                pending.insert(
                    id,
                    Pending {
                        request,
                        kind: ReplyKind::Read,
                    },
                );
                self.slot.read_for_request(self.owner, id)
            }
        };
        if let Err(error) = result {
            pending.remove(&id);
            return Err(store_error(error));
        }
        Ok(())
    }

    pub(crate) fn revoke(&self) {
        if let Some(pilot) = self.pilot.get()
            && pilot.begin_retirement().is_err()
        {
            tracing::warn!("native pilot retirement unavailable");
        }
        self.cancelled.cancel();
        if let Err(error) = self.slot.revoke() {
            tracing::warn!(%error, "observation revoke unavailable");
        }
    }

    /// The existing native task owner retains/polls this future. One receiver,
    /// one outgoing writer; no broadcast, independent ACK task or body logging.
    pub(crate) fn relay(
        self: &Arc<Self>,
        mut events: mpsc::Receiver<ObservationEvent>,
        outgoing: Arc<OutgoingMessageSender>,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        let weak = Arc::downgrade(self);
        let cancelled = self.cancelled.clone();
        async move {
            loop {
                let event = tokio::select! { biased;
                    _ = cancelled.cancelled() => break,
                    event = events.recv() => match event { Some(event) => event, None => break },
                };
                let Some(bridge) = weak.upgrade() else { break };
                let send = async {
                    match event {
                        ObservationEvent::Control {
                            request_id,
                            metadata,
                        } => {
                            let pending = {
                                let mut pending = bridge.pending.lock().ok()?;
                                pending.remove(&request_id)?
                            };
                            let state = match metadata.status {
                                ObservationStatus::Current => ObservationPublicationState::Current,
                                ObservationStatus::Cleared => ObservationPublicationState::Cleared,
                                ObservationStatus::Expired => ObservationPublicationState::Expired,
                                ObservationStatus::Unavailable => return None,
                            };
                            if metadata.owner != bridge.owner {
                                return None;
                            }
                            let response = ThreadObservationSetResponse {
                                owner_epoch: metadata.owner.epoch.to_string(),
                                revision: metadata.revision,
                                hash: metadata.hash,
                                state,
                                expires_at: metadata.expires_at,
                                commit_order: metadata.commit_order,
                            };
                            let response = match pending.kind {
                                ReplyKind::Set => {
                                    ClientResponsePayload::ThreadObservationSet(response)
                                }
                                ReplyKind::Read => {
                                    ClientResponsePayload::ThreadObservationRead(response)
                                }
                            };
                            outgoing
                                .send_response_as(pending.request, response)
                                .await
                                .then_some(())?;
                        }
                        event
                        @ (ObservationEvent::Captured(_) | ObservationEvent::Submitted(_)) => {
                            let notification = notification(bridge.thread_id, bridge.owner, event)?;
                            outgoing
                                .send_server_notification_to_connections(
                                    &[ConnectionId(bridge.owner.connection_id)],
                                    notification,
                                )
                                .await
                                .then_some(())?;
                        }
                        ObservationEvent::Published(_)
                        | ObservationEvent::Read(_)
                        | ObservationEvent::Budget { .. } => return None,
                    }
                    Some(())
                };
                if tokio::select! { biased; _ = cancelled.cancelled() => None, result = send => result }.is_none() { break; }
            }
            if let Some(bridge) = weak.upgrade() {
                bridge.revoke();
            }
            // Any committed but undelivered response remains uncertain. Never
            // turn relay/correlation/serialization failure into a domain rejection.
        }
    }
}

#[cfg(test)]
#[path = "observation_bridge_tests.rs"]
mod tests;

impl Drop for ObservationBridge {
    fn drop(&mut self) {
        self.revoke();
    }
}
