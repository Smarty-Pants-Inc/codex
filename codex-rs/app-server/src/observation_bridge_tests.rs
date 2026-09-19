use super::*;
use crate::outgoing_message::OutgoingEnvelope;
use crate::outgoing_message::OutgoingMessage;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerNotification;
use codex_core::ObservationError;
use pretty_assertions::assert_eq;
use std::future::Future;
use std::task::Context;
use std::task::Waker;
use std::time::Duration;

fn request(id: i64) -> ConnectionRequestId {
    ConnectionRequestId {
        connection_id: ConnectionId(42),
        request_id: RequestId::Integer(id),
    }
}

#[tokio::test]
async fn one_relay_orders_correlated_acks_capture_and_terminal_to_owner_only() {
    let thread_id = ThreadId::new();
    let (bridge, events) =
        ObservationBridge::new(ConnectionOrigin::Stdio, ConnectionId(42), thread_id).unwrap();
    let epoch = bridge.owner.epoch.to_string();
    bridge
        .submit(request(1), &epoch, ControlOperation::Read)
        .unwrap();
    let capture = bridge.slot.capture("turn-a").unwrap();
    bridge
        .submit(
            request(2),
            &epoch,
            ControlOperation::Set {
                revision: 1,
                frame: None,
            },
        )
        .unwrap();
    bridge.slot.release(capture.decision_id).unwrap();
    let (tx, mut rx) = mpsc::channel(4);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let relay = tokio::spawn(bridge.relay(events, outgoing));
    let mut order = Vec::new();
    for _ in 0..4 {
        let envelope = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let OutgoingEnvelope::ToConnection {
            connection_id,
            message,
            ..
        } = envelope
        else {
            panic!("owner-only output")
        };
        let (label, commit) = match message {
            OutgoingMessage::Response(response) => match *response.result {
                ClientResponsePayload::ThreadObservationRead(metadata) => {
                    assert_eq!(response.id, request(1).request_id);
                    ("read", metadata.commit_order)
                }
                ClientResponsePayload::ThreadObservationSet(metadata) => {
                    assert_eq!(response.id, request(2).request_id);
                    ("set", metadata.commit_order)
                }
                other => panic!("unexpected response: {other:?}"),
            },
            OutgoingMessage::AppServerNotification(envelope) => match envelope.notification {
                ServerNotification::ThreadObservationCaptured(notification) => {
                    assert_eq!(
                        (
                            notification.thread_id,
                            notification.turn_id,
                            notification.owner_epoch,
                            notification.decision_id
                        ),
                        (
                            thread_id.to_string(),
                            "turn-a".into(),
                            epoch.clone(),
                            capture.decision_id.to_string()
                        )
                    );
                    ("captured", notification.commit_order)
                }
                ServerNotification::ThreadObservationSubmitted(notification) => {
                    assert!(notification.terminal_decision);
                    assert_eq!(notification.decision_id, capture.decision_id.to_string());
                    ("submitted", notification.commit_order)
                }
                other => panic!("unexpected notification: {other:?}"),
            },
            other => panic!("unexpected outgoing message: {other:?}"),
        };
        order.push((connection_id, label, commit));
    }
    assert_eq!(
        order,
        vec![
            (ConnectionId(42), "read", 0),
            (ConnectionId(42), "captured", 1),
            (ConnectionId(42), "set", 2),
            (ConnectionId(42), "submitted", 3)
        ]
    );
    assert!(bridge.pending.lock().unwrap().is_empty());
    bridge.revoke();
    relay.await.unwrap();
}

#[test]
fn foreign_origin_owner_and_capacity_reject_before_slot_mutation() {
    for origin in [
        ConnectionOrigin::InProcess,
        ConnectionOrigin::WebSocket,
        ConnectionOrigin::RemoteControl,
    ] {
        assert_eq!(
            ObservationBridge::new(origin, ConnectionId(42), ThreadId::new())
                .err()
                .unwrap(),
            rejected(Code::Denied)
        );
    }
    let (bridge, _events) =
        ObservationBridge::new(ConnectionOrigin::Stdio, ConnectionId(42), ThreadId::new()).unwrap();
    let epoch = bridge.owner.epoch.to_string();
    let mut foreign = request(1);
    foreign.connection_id = ConnectionId(43);
    assert_eq!(
        bridge.submit(foreign, &epoch, ControlOperation::Read),
        Err(rejected(Code::Denied))
    );
    assert_eq!(
        bridge.submit(request(1), "wrong-epoch", ControlOperation::Read),
        Err(rejected(Code::StaleOwner))
    );
    assert_eq!(
        bridge.submit(
            request(1),
            &epoch,
            ControlOperation::Set {
                revision: 0,
                frame: None
            }
        ),
        Err(rejected(Code::RevisionMismatch))
    );
    assert!(bridge.pending.lock().unwrap().is_empty());
    for id in 0..32 {
        bridge
            .submit(request(id), &epoch, ControlOperation::Read)
            .unwrap();
    }
    assert_eq!(
        bridge.submit(request(33), &epoch, ControlOperation::Read),
        Err(rejected(Code::ResourceLimit))
    );
}

#[tokio::test]
async fn closing_an_unsubscribed_owner_revokes_only_its_binding() {
    use crate::thread_state::ConnectionCapabilities;
    use crate::thread_state::ThreadStateManager;
    let manager = ThreadStateManager::new();
    let thread_id = ThreadId::new();
    let (bridge, _events) =
        ObservationBridge::new(ConnectionOrigin::Stdio, ConnectionId(42), thread_id).unwrap();
    manager
        .connection_initialized(ConnectionId(42), ConnectionCapabilities::default())
        .await;
    manager
        .try_add_connection_to_thread(thread_id, ConnectionId(42))
        .await;
    manager
        .thread_state(thread_id)
        .await
        .lock()
        .await
        .observation = Some(bridge.clone());
    assert!(
        manager
            .unsubscribe_connection_from_thread(thread_id, ConnectionId(42))
            .await
    );
    let other_thread = ThreadId::new();
    let (other, _other_events) =
        ObservationBridge::new(ConnectionOrigin::Stdio, ConnectionId(43), other_thread).unwrap();
    manager
        .thread_state(other_thread)
        .await
        .lock()
        .await
        .observation = Some(other.clone());
    manager.revoke_observations(ConnectionId(42)).await;
    assert_eq!(
        bridge.submit(
            request(1),
            &bridge.owner.epoch.to_string(),
            ControlOperation::Read
        ),
        Err(rejected(Code::StaleOwner))
    );
    other
        .submit(
            ConnectionRequestId {
                connection_id: ConnectionId(43),
                request_id: RequestId::Integer(1),
            },
            &other.owner.epoch.to_string(),
            ControlOperation::Read,
        )
        .unwrap();
}

#[tokio::test]
async fn cancelled_relay_does_not_issue_a_postcommit_rejection() {
    let (bridge, events) =
        ObservationBridge::new(ConnectionOrigin::Stdio, ConnectionId(42), ThreadId::new()).unwrap();
    bridge
        .submit(
            request(1),
            &bridge.owner.epoch.to_string(),
            ControlOperation::Set {
                revision: 1,
                frame: None,
            },
        )
        .unwrap();
    bridge.revoke();
    let (tx, mut rx) = mpsc::channel(1);
    bridge
        .relay(
            events,
            Arc::new(OutgoingMessageSender::new(
                tx,
                codex_analytics::AnalyticsEventsClient::disabled(),
            )),
        )
        .await;
    assert!(rx.try_recv().is_err());
    assert_eq!(bridge.pending.lock().unwrap().len(), 1); // Lost ACK, not a rejected publication.
}

#[tokio::test]
async fn closed_outgoing_queue_stops_ack_and_notification_relay() {
    #[derive(Clone, Copy, Debug)]
    enum Output {
        Ack,
        Notification,
    }
    #[derive(Clone, Copy, Debug)]
    enum Closure {
        BeforeSend,
        WhileBlocked,
    }
    for output in [Output::Ack, Output::Notification] {
        for closure in [Closure::BeforeSend, Closure::WhileBlocked] {
            let (bridge, events) =
                ObservationBridge::new(ConnectionOrigin::Stdio, ConnectionId(42), ThreadId::new())
                    .unwrap();
            let epoch = bridge.owner.epoch.to_string();
            if matches!(closure, Closure::WhileBlocked) {
                // This healthy ACK fills the one-envelope outgoing queue.
                bridge
                    .submit(request(/*id*/ 0), &epoch, ControlOperation::Read)
                    .unwrap();
            }
            let capture = match output {
                Output::Ack => {
                    bridge
                        .submit(
                            request(/*id*/ 1),
                            &epoch,
                            ControlOperation::Set {
                                revision: 1,
                                frame: None,
                            },
                        )
                        .unwrap();
                    None
                }
                Output::Notification => Some(bridge.slot.capture("turn-a").unwrap()),
            };
            // This already committed read must remain unresolved behind the failure.
            bridge
                .submit(request(/*id*/ 2), &epoch, ControlOperation::Read)
                .unwrap();
            let (tx, mut rx) = mpsc::channel(/*buffer*/ 1);
            let outgoing = Arc::new(OutgoingMessageSender::new(
                tx,
                codex_analytics::AnalyticsEventsClient::disabled(),
            ));
            let mut relay = Box::pin(bridge.relay(events, outgoing));
            if matches!(closure, Closure::WhileBlocked) {
                // Poll to the full-queue send, not a sleep or spawned-task race.
                assert!(
                    relay
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                        .is_pending(),
                    "{output:?} {closure:?}"
                );
                assert_eq!(rx.len(), 1);
                assert!(!bridge.cancelled.is_cancelled());
            }
            rx.close();
            tokio::time::timeout(Duration::from_secs(/*secs*/ 1), relay.as_mut())
                .await
                .expect("failed enqueue must terminate the sole relay");
            drop(relay);
            assert!(bridge.cancelled.is_cancelled());
            assert_eq!(
                bridge
                    .pending
                    .lock()
                    .unwrap()
                    .values()
                    .map(|entry| entry.request.clone())
                    .collect::<Vec<_>>(),
                vec![request(/*id*/ 2)]
            );
            if matches!(closure, Closure::WhileBlocked) {
                let OutgoingEnvelope::ToConnection {
                    connection_id,
                    message: OutgoingMessage::Response(response),
                    ..
                } = rx.try_recv().unwrap()
                else {
                    panic!("only the earlier successful ACK may be queued")
                };
                assert_eq!(connection_id, ConnectionId(42));
                assert_eq!(response.id, request(/*id*/ 0).request_id);
                assert!(matches!(
                    *response.result,
                    ClientResponsePayload::ThreadObservationRead(_)
                ));
            }
            // Neither the failed send nor the unresolved read gets a false ACK/error.
            assert!(rx.try_recv().is_err());
            assert_eq!(
                bridge.submit(request(/*id*/ 3), &epoch, ControlOperation::Read),
                Err(rejected(Code::StaleOwner))
            );
            assert_eq!(
                bridge
                    .slot
                    .set(bridge.owner, /*revision*/ 2, /*frame*/ None),
                Err(ObservationError::StaleOwner)
            );
            if let Some(capture) = capture {
                bridge.slot.release(capture.decision_id).unwrap();
            }
            // No active decision remains: refusal now fences a fresh capture.
            assert_eq!(
                bridge.slot.capture("turn-after-failure"),
                Err(ObservationError::ResourceLimit)
            );
        }
    }
}
