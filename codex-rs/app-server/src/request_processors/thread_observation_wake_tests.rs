use super::*;
use crate::connection_rpc_gate::ConnectionRpcGate;
use crate::observation_bridge::ObservationBridge;
use crate::outgoing_message::ConnectionId;
use crate::request_serialization::QueuedInitializedRequest;
use crate::request_serialization::RequestSerializationQueueKey;
use crate::request_serialization::RequestSerializationQueues;
use crate::transport::ConnectionOrigin;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::WakeIntent;
use codex_core::IdleTurnAdmission;
use codex_core::ObservationBinding;
use codex_core::ObservationProfile;
use codex_protocol::ThreadId;
use core_test_support::responses;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Debug)]
struct Denied;
impl IdleTurnAdmission for Denied {
    fn reserve_if_allowed(&self, _reserve: &mut dyn FnMut()) -> bool {
        false
    }
}

#[derive(Debug)]
struct HeldPolicy {
    entered: Arc<Notify>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}
impl IdleTurnAdmission for HeldPolicy {
    fn reserve_if_allowed(&self, reserve: &mut dyn FnMut()) -> bool {
        self.entered.notify_one();
        if self
            .release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .is_err()
        {
            return false;
        }
        reserve();
        true
    }
}

// Exercise the actual dispatch selector, existing queue and connection task gate.
// The fixture supplies only the existing handler future, not a new production seam.
async fn dispatch(
    queues: &RequestSerializationQueues,
    gate: &Arc<ConnectionRpcGate>,
    request: ClientRequest,
    future: impl std::future::Future<Output = ()> + Send + 'static,
) {
    let queued = QueuedInitializedRequest::new(Arc::clone(gate), future);
    if let Some(scope) = request.serialization_scope() {
        let (key, access) = RequestSerializationQueueKey::from_scope(ConnectionId(42), scope);
        queues.enqueue(key, access, queued).await;
    } else {
        tokio::spawn(queued.run());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trusted_policy_validation_receipts_and_inflight_cleanup_use_original_native_paths()
-> anyhow::Result<()> {
    let server = responses::start_mock_server().await;
    let test = test_codex()
        .with_config(|config| {
            config.model = Some("gpt-oss-20b".into());
            let mut model = codex_models_manager::model_info::model_info_from_slug("gpt-oss-20b");
            model.context_window = Some(131_072);
            model.effective_context_window_percent = 100;
            config.model_catalog = Some(codex_protocol::openai_models::ModelsResponse {
                models: vec![model],
            });
        })
        .build_with_auto_env(&server)
        .await?;
    let thread_id = ThreadId::from_string(test.codex.thread_extension_data().level_id())?;
    let (bridge, _events) =
        ObservationBridge::new(ConnectionOrigin::Stdio, ConnectionId(42), thread_id)
            .expect("original bridge");
    test.codex
        .install_budgeted_observation_binding(
            ObservationBinding {
                slot: Arc::clone(&bridge.slot),
                profile: ObservationProfile::HarmonyGptOss,
            },
            bridge.owner,
        )
        .await?;
    let hash = format!("{:x}", Sha256::digest(b"current"));
    let expiry = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    )? + 60;
    bridge.slot.set_for_request_at_budget(
        bridge.owner,
        /*revision*/ 1,
        Some(codex_core::ObservationFrame {
            text: Arc::from("current"),
            hash: hash.clone(),
            expires_at: expiry,
        }),
        uuid::Uuid::new_v4(),
        /*budget_generation*/ 1,
    )?;
    let native = ObservationWakeIntent {
        sequence: 1,
        frame_revision: 1,
        frame_hash: hash,
        budget_generation: 1,
        expected_commit_order: 1,
    };
    let params = ThreadObservationWakeStartParams {
        thread_id: thread_id.to_string(),
        owner_epoch: bridge.owner.epoch.to_string(),
        intent: WakeIntent {
            sequence: 1,
            frame_revision: 1,
            frame_hash: native.frame_hash.clone(),
            budget_generation: 1,
            expected_commit_order: 1,
        },
        operand_digest: native.operand_digest(bridge.owner),
    };
    let policy = ObservationWakeHostPolicy {
        thread_id,
        owner: bridge.owner,
        admission: Arc::new(Denied),
    };
    test.codex.thread_extension_data().insert(policy.clone());
    for change in [0, 1, 2] {
        let mut bad = params.clone();
        match change {
            0 => bad.intent.sequence = 1_u64 << 53,
            1 => bad.intent.frame_hash = "A".repeat(64),
            2 => bad.operand_digest = "0".repeat(64),
            _ => unreachable!(),
        }
        assert_eq!(
            start(&bridge, &test.codex, bad).await.unwrap_err(),
            rejected(Code::InvalidInput)
        );
        assert_eq!(
            bridge
                .slot
                .observation_wake_snapshot(bridge.owner)?
                .intent_floor,
            0
        );
    }
    for change_owner in [false, true] {
        let mut wrong = policy.clone();
        if change_owner {
            wrong.owner.epoch = uuid::Uuid::new_v4();
        } else {
            wrong.thread_id = ThreadId::new();
        }
        test.codex.thread_extension_data().insert(wrong);
        assert_eq!(
            start(&bridge, &test.codex, params.clone())
                .await
                .unwrap_err(),
            rejected(Code::Denied)
        );
        assert_eq!(
            bridge
                .slot
                .observation_wake_snapshot(bridge.owner)?
                .intent_floor,
            0
        );
    }
    test.codex.thread_extension_data().insert(policy.clone());
    let result = start(&bridge, &test.codex, params.clone())
        .await
        .expect("denied policy result");
    let expected =
        json!({"sequence":1,"operandDigest":params.operand_digest,"outcome":{"type":"suppressed"}});
    assert_eq!(
        serde_json::to_value(result)?,
        json!({"protocol":1,"receipt":expected})
    );
    let read = ThreadObservationWakeReadParams {
        thread_id: thread_id.to_string(),
        owner_epoch: params.owner_epoch.clone(),
        query: WakeReadQuery::Attempt {
            sequence: 1,
            operand_digest: params.operand_digest.clone(),
        },
    };
    assert_eq!(
        serde_json::to_value(
            control(&bridge, WakeOperation::Read(read.clone())).expect("exact readback")
        )?,
        json!({"protocol":1,"type":"attempt","intentFloor":1,"receipt":expected})
    );
    let mut conflict = read.clone();
    conflict.query = WakeReadQuery::Attempt {
        sequence: 1,
        operand_digest: "0".repeat(64),
    };
    assert_eq!(
        control(&bridge, WakeOperation::Read(conflict)).unwrap_err(),
        rejected(Code::RevisionMismatch)
    );
    control(
        &bridge,
        WakeOperation::Retire(ThreadObservationWakeRetireParams {
            thread_id: thread_id.to_string(),
            owner_epoch: params.owner_epoch.clone(),
            sequence: 1,
            operand_digest: params.operand_digest.clone(),
        }),
    )
    .expect("terminal retirement");

    let entered = Arc::new(Notify::new());
    let (release, receiver) = std::sync::mpsc::channel();
    test.codex
        .thread_extension_data()
        .insert(ObservationWakeHostPolicy {
            admission: Arc::new(HeldPolicy {
                entered: Arc::clone(&entered),
                release: Mutex::new(receiver),
            }),
            ..policy
        });
    let mut pending = params.clone();
    pending.intent.sequence = 2;
    let mut next_native = native;
    next_native.sequence = 2;
    pending.operand_digest = next_native.operand_digest(bridge.owner);
    let queues = RequestSerializationQueues::default();
    let gate = Arc::new(ConnectionRpcGate::new());
    let (finished, result) = tokio::sync::oneshot::channel();
    let original_bridge = Arc::clone(&bridge);
    let original_thread = Arc::clone(&test.codex);
    let wire_start = ClientRequest::ThreadObservationWakeStart {
        id: RequestId::Integer(1),
        params: pending.clone(),
    };
    dispatch(&queues, &gate, wire_start, async move {
        let _ = finished.send(start(&original_bridge, &original_thread, pending).await);
    })
    .await;
    tokio::time::timeout(Duration::from_secs(5), entered.notified()).await?;
    // The start holds native admission in the original policy, but read/fence
    // dispatch must still run. A thread-serialized cleanup would time out here.
    let invalidate = ThreadObservationWakeInvalidateParams {
        thread_id: thread_id.to_string(),
        owner_epoch: params.owner_epoch.clone(),
        sequence: 3,
    };
    let (sent, fence_result) = tokio::sync::oneshot::channel();
    let original_bridge = Arc::clone(&bridge);
    dispatch(
        &queues,
        &gate,
        ClientRequest::ThreadObservationWakeInvalidate {
            id: RequestId::Integer(2),
            params: invalidate.clone(),
        },
        async move {
            let _ = sent.send(control(
                &original_bridge,
                WakeOperation::Invalidate(invalidate),
            ));
        },
    )
    .await;
    assert_eq!(
        serde_json::to_value(
            tokio::time::timeout(Duration::from_secs(5), fence_result)
                .await??
                .expect("fence result")
        )?,
        json!({"protocol":1,"intentFloor":3})
    );
    let (sent, read_result) = tokio::sync::oneshot::channel();
    let original_bridge = Arc::clone(&bridge);
    let read = ThreadObservationWakeReadParams {
        query: WakeReadQuery::Fence { sequence: 3 },
        ..read
    };
    dispatch(
        &queues,
        &gate,
        ClientRequest::ThreadObservationWakeRead {
            id: RequestId::Integer(3),
            params: read.clone(),
        },
        async move {
            let _ = sent.send(control(&original_bridge, WakeOperation::Read(read)));
        },
    )
    .await;
    assert_eq!(
        serde_json::to_value(
            tokio::time::timeout(Duration::from_secs(5), read_result)
                .await??
                .expect("read result")
        )?,
        json!({"protocol":1,"type":"fence","intentFloor":3})
    );
    let closing = Arc::clone(&gate);
    let shutdown = tokio::spawn(async move {
        closing.shutdown().await;
    });
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished());
    release.send(())?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), result)
            .await??
            .expect("original start result")
            .receipt
            .outcome,
        WakeOutcome::Suppressed
    );
    tokio::time::timeout(Duration::from_secs(5), shutdown).await??;
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
