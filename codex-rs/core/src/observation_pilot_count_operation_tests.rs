use super::*;
use crate::observation::ObservationOwner;
use crate::observation::pilot::NativePilotIssuer;
use crate::observation::pilot::PilotGrantClaims;
use crate::observation::pilot::PilotLedgerIdentity;
use crate::observation::pilot::PilotPermission;
use codex_http_client::TransportError;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;
use std::fs::OpenOptions;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[derive(Clone, Copy)]
enum Reply {
    Complete,
    Oversize,
    Malformed,
    OverBudget,
    Pending,
    TamperFinal,
    Revoke,
}

pub(in crate::observation::pilot::count) struct ControlledIo {
    slot: Arc<ObservationSlot>,
    journal: std::path::PathBuf,
    mode: Reply,
    bodies: Mutex<Vec<Vec<u8>>>,
    entered: tokio::sync::Notify,
}
impl ControlledIo {
    pub(super) async fn send(
        &self,
        mut request: Request,
        gate: impl FnOnce(&Request) -> Result<(), String> + Send,
    ) -> Result<StreamResponse, TransportError> {
        assert!(
            self.slot.state.try_lock().is_ok(),
            "slot lock held during transport"
        );
        let count = request.url.ends_with("/input_tokens");
        let journal = std::fs::read_to_string(&self.journal).unwrap();
        assert_eq!(journal.lines().count(), if count { 1 } else { 2 });
        if !count && matches!(self.mode, Reply::TamperFinal) {
            request.body = Some(RequestBody::EncodedJson(
                codex_client::EncodedJsonBody::encode(&serde_json::json!({"mutated":true}))
                    .unwrap(),
            ));
        }
        gate(&request).map_err(TransportError::Build)?;
        let body = request.prepare_body_for_send().unwrap().body.unwrap();
        self.bodies.lock().unwrap().push(body.to_vec());
        self.entered.notify_one();
        if count && matches!(self.mode, Reply::Pending) {
            std::future::pending::<()>().await;
        }
        if count && matches!(self.mode, Reply::Revoke) {
            self.slot
                .state
                .lock()
                .unwrap()
                .pilot
                .as_mut()
                .unwrap()
                .revoke();
        }
        let bytes = if count {
            match self.mode {
                Reply::Oversize => vec![b'x'; 4097],
                Reply::OverBudget => {
                    b"{\"object\":\"response.input_tokens\",\"input_tokens\":100}".to_vec()
                }
                Reply::Malformed => {
                    b"{\"object\":\"response.input_tokens\",\"input_tokens\":1e1}".to_vec()
                }
                Reply::Complete | Reply::Pending | Reply::TamperFinal | Reply::Revoke => {
                    b"{\"object\":\"response.input_tokens\",\"input_tokens\":7}".to_vec()
                }
            }
        } else {
            Vec::new()
        };
        Ok(StreamResponse {
            status: http::StatusCode::OK,
            headers: http::HeaderMap::new(),
            bytes: Box::pin(futures::stream::iter(vec![Ok(bytes.into())])),
        })
    }
}

struct Issuer {
    claims: PilotGrantClaims,
    scope: PilotCountScope,
    journal: Mutex<Option<PilotCountJournal>>,
    revoked: AtomicBool,
    credential: Arc<()>,
}
#[derive(Clone)]
struct Credential(Arc<()>);
impl NativePilotIssuer for Issuer {
    fn verify_grant(&self, envelope: &[u8]) -> Result<PilotGrantClaims, PilotAuthorityError> {
        if envelope != b"fixture" {
            return Err(PilotAuthorityError::Denied);
        }
        Ok(self.claims.clone())
    }
    fn recheck_grant(&self, claims: &PilotGrantClaims) -> Result<(), PilotAuthorityError> {
        if self.revoked.load(Ordering::Acquire) || claims.grant_id != self.claims.grant_id {
            return Err(PilotAuthorityError::Expired);
        }
        Ok(())
    }
    fn revoke(&self) {
        self.revoked.store(/*val*/ true, Ordering::Release);
    }
    fn authenticate_request(
        &self,
        claims: &PilotGrantClaims,
        request: &mut Request,
    ) -> Result<codex_client::RequestAuthentication, PilotAuthorityError> {
        self.recheck_grant(claims)?;
        request
            .extensions
            .insert(Credential(Arc::clone(&self.credential)));
        Ok(codex_client::RequestAuthentication::Prepared)
    }
    fn qualify_request(
        &self,
        _: &PilotGrantClaims,
        _: &Request,
    ) -> Result<PilotRequestReservation, PilotAuthorityError> {
        Err(PilotAuthorityError::Unavailable)
    }
    fn take_count_journal(
        &self,
        _: &PilotGrantClaims,
    ) -> Result<Option<PilotCountJournal>, PilotAuthorityError> {
        Ok(self.journal.lock().unwrap().take())
    }
    fn count_scope(
        &self,
        _: &PilotGrantClaims,
    ) -> Result<Option<PilotCountScope>, PilotAuthorityError> {
        Ok(Some(self.scope.clone()))
    }
    fn validate_count_inference(
        &self,
        claims: &PilotGrantClaims,
        request: &Request,
    ) -> Result<(), PilotAuthorityError> {
        self.recheck_grant(claims)?;
        if !request
            .extensions
            .get::<Credential>()
            .is_some_and(|c| Arc::ptr_eq(&c.0, &self.credential))
        {
            return Err(PilotAuthorityError::Denied);
        }
        Ok(())
    }
    fn authenticate_count_request(
        &self,
        claims: &PilotGrantClaims,
        request: &mut Request,
    ) -> Result<(), PilotAuthorityError> {
        if request.url != self.scope.count_url {
            return Err(PilotAuthorityError::Denied);
        }
        self.authenticate_request(claims, request).map(|_| ())
    }
    fn count_transport_factory(
        &self,
        _: &PilotGrantClaims,
    ) -> Result<codex_http_client::HttpClientFactory, PilotAuthorityError> {
        Ok(codex_http_client::HttpClientFactory::new(
            codex_http_client::OutboundProxyPolicy::ReqwestDefault,
        ))
    }
}

struct Fixture {
    slot: Arc<ObservationSlot>,
    owner: ObservationOwner,
    request: Request,
    wire: Arc<CountWire>,
    decision: Uuid,
    io: Arc<ControlledIo>,
    _events: tokio::sync::mpsc::Receiver<crate::observation::ObservationEvent>,
    _home: tempfile::TempDir,
}
impl Fixture {
    fn new(mode: Reply) -> anyhow::Result<Self> {
        let now = Instant::now();
        let (slot, events, owner) = ObservationSlot::with_clock(
            /*connection_id*/ 7,
            Box::new(move || {
                Ok(ObservationClock {
                    wall_seconds: 1000,
                    monotonic: now,
                })
            }),
        );
        let slot = Arc::new(slot);
        let home = tempfile::tempdir()?;
        let path = home.path().join("ledger");
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        let metadata = file.metadata()?;
        let inputs: Vec<_> = (0..5)
            .map(|_| tempfile::tempfile())
            .collect::<Result<_, _>>()?;
        let journal = PilotCountJournal::receive(
            file,
            PilotLedgerIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
                uid: metadata.uid(),
                gid: metadata.gid(),
            },
            [&inputs[0], &inputs[1], &inputs[2], &inputs[3], &inputs[4]],
        )?;
        let claims = PilotGrantClaims {
            grant_id: Uuid::new_v4(),
            issuer_generation: 1,
            owner,
            thread_id: ThreadId::new(),
            scope: "fixture".into(),
            permissions: [PilotPermission::ForegroundTurn].into(),
            expires_at: 1060,
            cooldown: Duration::ZERO,
            max_turns: 0,
            max_attempts: 8,
            max_reserved_tokens: NonZeroU64::new(/*n*/ 800).unwrap(),
        };
        let scope = PilotCountScope {
            allocation_id: "fixture-allocation".into(),
            instruction: "fixture-instruction".into(),
            decision_sha256: "a".repeat(/*n*/ 64),
            credential_generation: "delivery-9".into(),
            credential_receipt: Uuid::new_v4(),
            semantics_sha256: "b".repeat(/*n*/ 64),
            wire_model: "fixture-model".into(),
            provider: "fixture-provider".into(),
            purpose: "fixture-purpose".into(),
            account: "fixture-account".into(),
            inference_url: "https://fixture.invalid/responses".into(),
            count_url: "https://fixture.invalid/responses/input_tokens".into(),
            not_before: 999,
            expires_at: 1060,
            operations: 8,
            output_tokens: NonZeroU64::new(/*n*/ 17).unwrap(),
            context_tokens: NonZeroU64::new(/*n*/ 100).unwrap(),
        };
        let issuer = Arc::new(Issuer {
            claims: claims.clone(),
            scope,
            journal: Mutex::new(Some(journal)),
            revoked: AtomicBool::new(/*v*/ false),
            credential: Arc::new(()),
        });
        slot.install_pilot_authority(owner, claims.thread_id, b"fixture", issuer.clone())?;
        let wire = Arc::new(codex_api::prepare_response_count(
            &codex_api::ResponsesApiRequest {
                model: "fixture-model".into(),
                instructions: "fixture instructions".into(),
                input: vec![],
                tools: None,
                tool_choice: "auto".into(),
                parallel_tool_calls: false,
                reasoning: None,
                store: false,
                stream: true,
                stream_options: None,
                include: vec![],
                service_tier: None,
                prompt_cache_key: None,
                text: None,
                client_metadata: None,
            },
            NonZeroU64::new(/*n*/ 17).unwrap(),
        )?);
        let io = Arc::new(ControlledIo {
            slot: Arc::clone(&slot),
            journal: path,
            mode,
            bodies: Mutex::new(Vec::new()),
            entered: tokio::sync::Notify::new(),
        });
        let mut request = Request::new(http::Method::POST, issuer.scope.inference_url.clone());
        request.body = Some(RequestBody::EncodedJson(wire.inference_body().clone()));
        request.extensions.insert(Arc::clone(&wire));
        request.extensions.insert(Arc::clone(&io));
        issuer.authenticate_request(&claims, &mut request)?;
        let capture = slot.capture("fixture-turn")?;
        Ok(Self {
            slot,
            owner,
            request,
            wire,
            decision: capture.decision_id,
            io,
            _events: events,
            _home: home,
        })
    }
}

#[tokio::test]
async fn original_async_count_syncs_before_each_send_and_consumes_exact_final_bytes_once()
-> anyhow::Result<()> {
    let fixture = Fixture::new(Reply::Complete)?;
    let (stream, attempt) = fixture
        .slot
        .stream_counted_request(fixture.decision, &fixture.request)
        .await?;
    assert_eq!(
        *fixture.io.bodies.lock().unwrap(),
        vec![
            fixture.wire.count_body().as_bytes().to_vec(),
            fixture.wire.inference_body().as_bytes().to_vec()
        ]
    );
    let report = fixture.slot.pilot_report(fixture.owner)?;
    assert_eq!(
        (
            report.reserved_tokens,
            report.attempts.len(),
            report.attempts[0].attempt_id
        ),
        (100, 1, attempt.attempt_id)
    );
    drop(stream);
    // Unknown inference completion does not authorize an automatic replay.
    assert!(
        fixture
            .slot
            .stream_counted_request(fixture.decision, &fixture.request)
            .await
            .is_err()
    );
    let mut expected = report;
    expected.revoked = true;
    assert_eq!(fixture.slot.pilot_report(fixture.owner)?, expected);
    assert_eq!(fixture.io.bodies.lock().unwrap().len(), 2);
    Ok(())
}

#[tokio::test]
async fn malformed_oversize_revoked_and_mutated_requests_never_send_inference_or_refund()
-> anyhow::Result<()> {
    for mode in [
        Reply::Oversize,
        Reply::Malformed,
        Reply::OverBudget,
        Reply::Revoke,
        Reply::TamperFinal,
    ] {
        let fixture = Fixture::new(mode)?;
        assert!(
            fixture
                .slot
                .stream_counted_request(fixture.decision, &fixture.request)
                .await
                .is_err()
        );
        let report = fixture.slot.pilot_report(fixture.owner)?;
        assert_eq!(
            (report.revoked, report.reserved_tokens, report.attempts),
            (true, 100, vec![])
        );
        assert_eq!(fixture.io.bodies.lock().unwrap().len(), 1);
        assert!(
            fixture
                .slot
                .stream_counted_request(fixture.decision, &fixture.request)
                .await
                .is_err()
        );
        assert_eq!(fixture.io.bodies.lock().unwrap().len(), 1);
    }
    Ok(())
}

#[tokio::test]
async fn counted_usage_limits_fence_even_below_the_total_reservation() -> anyhow::Result<()> {
    let fixture = Fixture::new(Reply::Complete)?;
    let (stream, attempt) = fixture
        .slot
        .stream_counted_request(fixture.decision, &fixture.request)
        .await?;
    let usage = codex_protocol::protocol::TokenUsage {
        input_tokens: 8,
        output_tokens: 1,
        total_tokens: 9,
        ..Default::default()
    };
    assert_eq!(
        fixture
            .slot
            .pilot_attempt_completed(attempt, "fixture-response", Some(&usage)),
        Err(PilotAuthorityError::Denied)
    );
    let report = fixture.slot.pilot_report(fixture.owner)?;
    assert_eq!(
        (
            report.revoked,
            report.reserved_tokens,
            report.attempts[0].usage_conflict
        ),
        (true, 100, true)
    );
    drop(stream);
    Ok(())
}

#[test]
fn durable_acknowledgement_cannot_transfer_between_allocations_or_repeat() -> anyhow::Result<()> {
    let first = Fixture::new(Reply::Complete)?;
    let other = Fixture::new(Reply::Complete)?;
    let plan = first.slot.prepare_count_attempt(
        first.decision,
        &first.request,
        Arc::clone(&first.wire),
    )?;
    let durable = plan.persist()?;
    assert_eq!(
        other.slot.acknowledge_count_debit(&durable),
        Err(PilotAuthorityError::Denied)
    );
    first.slot.acknowledge_count_debit(&durable)?;
    assert_eq!(
        first.slot.acknowledge_count_debit(&durable),
        Err(PilotAuthorityError::Replay)
    );
    assert_eq!(
        (
            first.slot.pilot_report(first.owner)?.reserved_tokens,
            other.slot.pilot_report(other.owner)?.reserved_tokens
        ),
        (100, 0)
    );
    assert!(first.io.bodies.lock().unwrap().is_empty());
    assert!(other.io.bodies.lock().unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn cancelled_count_retains_debit_and_retires_original_transport_custody() -> anyhow::Result<()>
{
    let fixture = Fixture::new(Reply::Pending)?;
    let slot = Arc::clone(&fixture.slot);
    let request = fixture.request.clone();
    let decision = fixture.decision;
    let task = tokio::spawn(async move { slot.stream_counted_request(decision, &request).await });
    fixture.io.entered.notified().await;
    task.abort();
    match task.await {
        Err(error) => assert!(error.is_cancelled()),
        Ok(_) => panic!("controlled pending operation completed"),
    }
    let report = fixture.slot.pilot_report(fixture.owner)?;
    assert_eq!(
        (report.revoked, report.reserved_tokens, report.attempts),
        (true, 100, vec![])
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.io.journal)?
            .lines()
            .count(),
        1
    );
    fixture.slot.wait_for_transport_drain().await;
    Ok(())
}
