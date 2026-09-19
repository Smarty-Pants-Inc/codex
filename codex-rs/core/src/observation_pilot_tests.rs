use super::*;
use crate::observation::ObservationError;
use crate::observation::ObservationEvent;
use codex_protocol::protocol::TokenUsage;
use pretty_assertions::assert_eq;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

// Only a seam fixture. This authenticates no real grant, credential or H context.
struct FixtureIssuer {
    claims: PilotGrantClaims,
    generation: AtomicU64,
}

impl NativePilotIssuer for FixtureIssuer {
    fn authenticate_request(
        &self,
        _: &PilotGrantClaims,
        _: &mut Request,
    ) -> Result<codex_client::RequestAuthentication, PilotAuthorityError> {
        Ok(codex_client::RequestAuthentication::Provider)
    }

    fn verify_grant(&self, envelope: &[u8]) -> Result<PilotGrantClaims, PilotAuthorityError> {
        if envelope != b"fixture-not-a-grant" {
            return Err(PilotAuthorityError::Denied);
        }
        Ok(self.claims.clone())
    }

    fn revoke(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn recheck_grant(&self, claims: &PilotGrantClaims) -> Result<(), PilotAuthorityError> {
        if self.generation.load(Ordering::SeqCst) != claims.issuer_generation {
            return Err(PilotAuthorityError::Expired);
        }
        Ok(())
    }

    fn qualify_request(
        &self,
        _: &PilotGrantClaims,
        request: &Request,
    ) -> Result<PilotRequestReservation, PilotAuthorityError> {
        if request
            .headers
            .get("x-fixture-context")
            .and_then(|value| value.to_str().ok())
            != Some("qualified-fixture")
        {
            return Err(PilotAuthorityError::Unavailable);
        }
        Ok(PilotRequestReservation {
            token_ceiling: NonZeroU64::new(/*n*/ 1000).unwrap(),
            credential_receipt: Uuid::new_v4(),
            context_receipt: Uuid::new_v4(),
        })
    }
}

struct Fixture {
    slot: Arc<ObservationSlot>,
    events: tokio::sync::mpsc::Receiver<ObservationEvent>,
    owner: ObservationOwner,
    clock: Arc<Mutex<ObservationClock>>,
    issuer: Arc<FixtureIssuer>,
}

impl Fixture {
    fn new() -> Self {
        let clock = Arc::new(Mutex::new(ObservationClock {
            wall_seconds: 1000,
            monotonic: Instant::now(),
        }));
        let source = Arc::clone(&clock);
        let (slot, events, owner) = ObservationSlot::with_clock(
            /*connection_id*/ 7,
            Box::new(move || Ok(*source.lock().unwrap())),
        );
        let claims = PilotGrantClaims {
            grant_id: Uuid::new_v4(),
            issuer_generation: 1,
            owner,
            thread_id: ThreadId::new(),
            scope: "fixture-scope".into(),
            permissions: [
                PilotPermission::SampleSource,
                PilotPermission::AutomaticTurn,
            ]
            .into(),
            expires_at: 1060,
            cooldown: Duration::from_secs(/*secs*/ 1),
            max_turns: 2,
            max_attempts: 2,
            max_reserved_tokens: NonZeroU64::new(/*n*/ 2000).unwrap(),
        };
        Self {
            slot: Arc::new(slot),
            events,
            owner,
            clock,
            issuer: Arc::new(FixtureIssuer {
                claims,
                generation: AtomicU64::new(/*v*/ 1),
            }),
        }
    }

    fn install(&self) {
        self.slot
            .install_pilot_authority(
                self.owner,
                self.issuer.claims.thread_id,
                b"fixture-not-a-grant",
                self.issuer.clone(),
            )
            .unwrap();
    }

    fn guard(&self, request_id: Uuid) -> Arc<dyn IdleTurnAdmission> {
        self.slot
            .pilot_turn_guard(self.owner, "fixture-scope".into(), request_id)
    }
}

#[test]
fn missing_wrong_owner_scope_permission_and_replay_are_rejected() {
    let fixture = Fixture::new();
    let nonce = Uuid::new_v4();
    assert!(!fixture.guard(nonce).reserve_turn_if_allowed(
        &fixture.issuer.claims.thread_id,
        "turn",
        &mut || {},
    ));
    assert_eq!(
        fixture.slot.install_pilot_authority(
            fixture.owner,
            ThreadId::new(),
            b"fixture-not-a-grant",
            fixture.issuer.clone()
        ),
        Err(PilotAuthorityError::Denied)
    );
    fixture.install();
    let before = fixture.slot.pilot_report(fixture.owner).unwrap();
    let wrong_owner = ObservationOwner {
        connection_id: fixture.owner.connection_id + 1,
        ..fixture.owner
    };
    for result in [
        fixture.slot.check_pilot_grant(
            wrong_owner,
            "fixture-scope",
            PilotPermission::SampleSource,
            nonce,
        ),
        fixture.slot.check_pilot_grant(
            fixture.owner,
            "wrong-scope",
            PilotPermission::SampleSource,
            nonce,
        ),
        fixture.slot.check_pilot_grant(
            fixture.owner,
            "fixture-scope",
            PilotPermission::ActOnSource,
            nonce,
        ),
        fixture.slot.check_pilot_grant(
            fixture.owner,
            "fixture-scope",
            PilotPermission::AutomaticTurn,
            nonce,
        ),
    ] {
        assert_eq!(result, Err(PilotAuthorityError::Denied));
    }
    assert_eq!(fixture.slot.pilot_report(fixture.owner).unwrap(), before);
    fixture
        .slot
        .check_pilot_grant(
            fixture.owner,
            "fixture-scope",
            PilotPermission::SampleSource,
            nonce,
        )
        .unwrap();
    assert_eq!(
        fixture.slot.check_pilot_grant(
            fixture.owner,
            "fixture-scope",
            PilotPermission::SampleSource,
            nonce
        ),
        Err(PilotAuthorityError::Replay)
    );
    assert!(!fixture.guard(nonce).reserve_turn_if_allowed(
        &fixture.issuer.claims.thread_id,
        "turn",
        &mut || {},
    ));
    fixture.issuer.generation.store(/*val*/ 2, Ordering::SeqCst);
    assert!(!fixture.guard(Uuid::new_v4()).reserve_turn_if_allowed(
        &fixture.issuer.claims.thread_id,
        "turn",
        &mut || {},
    ));
    fixture.issuer.generation.store(/*val*/ 1, Ordering::SeqCst);
    assert!(!fixture.guard(Uuid::new_v4()).reserve_turn_if_allowed(
        &fixture.issuer.claims.thread_id,
        "turn",
        &mut || {},
    ));
    let mut fenced = before;
    fenced.revoked = true;
    assert_eq!(fixture.slot.pilot_report(fixture.owner).unwrap(), fenced);
}

#[test]
fn commit_cooldown_native_identity_finite_spend_and_expiry_are_owner_atomic() {
    let fixture = Fixture::new();
    fixture.install();
    let thread = fixture.issuer.claims.thread_id;
    let nonce = Uuid::new_v4();
    let guard = fixture.guard(nonce);
    let mut reservations = 0;
    assert!(!guard.reserve_if_allowed(&mut || reservations += 1));
    assert!(
        !guard.reserve_turn_if_allowed(&ThreadId::new(), "wrong-thread", &mut || reservations += 1)
    );
    assert!(guard.reserve_turn_if_allowed(&thread, "turn-one", &mut || {
        assert!(fixture.slot.state.try_lock().is_err());
        reservations += 1;
    }));
    assert!(
        !guard
            .clone()
            .reserve_turn_if_allowed(&thread, "replay", &mut || reservations += 1)
    );
    let second_nonce = Uuid::new_v4();
    let second = fixture.guard(second_nonce);
    assert!(!second.reserve_turn_if_allowed(&thread, "turn-two", &mut || reservations += 1));
    fixture.clock.lock().unwrap().monotonic += Duration::from_secs(/*secs*/ 1);
    assert!(second.reserve_turn_if_allowed(&thread, "turn-two", &mut || reservations += 1));
    assert_eq!(reservations, 2);
    fixture.clock.lock().unwrap().monotonic += Duration::from_secs(/*secs*/ 1);
    assert!(!fixture.guard(Uuid::new_v4()).reserve_turn_if_allowed(
        &thread,
        "turn-three",
        &mut || {}
    ));
    let mut expected = fixture.slot.pilot_report(fixture.owner).unwrap();
    expected.admitted_turns = [
        ("turn-one".into(), nonce),
        ("turn-two".into(), second_nonce),
    ]
    .into();
    assert_eq!(fixture.slot.pilot_report(fixture.owner).unwrap(), expected);
    let original_clock = *fixture.clock.lock().unwrap();
    fixture.clock.lock().unwrap().monotonic += Duration::from_secs(/*secs*/ 60);
    assert_eq!(
        fixture.slot.check_pilot_grant(
            fixture.owner,
            "fixture-scope",
            PilotPermission::SampleSource,
            Uuid::new_v4()
        ),
        Err(PilotAuthorityError::Expired)
    );
    *fixture.clock.lock().unwrap() = original_clock;
    assert_eq!(
        fixture.slot.check_pilot_grant(
            fixture.owner,
            "fixture-scope",
            PilotPermission::SampleSource,
            Uuid::new_v4()
        ),
        Err(PilotAuthorityError::Expired)
    );
}

#[test]
fn auth_first_generation_failure_permanently_fences_auth_checks_and_admission() {
    let fixture = Fixture::new();
    fixture.install();
    let mut request = Request::new(
        http::Method::POST,
        "http://fixture.invalid/responses".into(),
    );
    fixture.issuer.generation.store(/*val*/ 2, Ordering::SeqCst);
    assert_eq!(
        fixture.slot.authenticate_pilot_request(&mut request),
        Err(PilotAuthorityError::Expired)
    );
    fixture.issuer.generation.store(/*val*/ 1, Ordering::SeqCst);
    assert_eq!(
        fixture.slot.authenticate_pilot_request(&mut request),
        Err(PilotAuthorityError::Expired)
    );
    assert_eq!(
        fixture.slot.check_pilot_grant(
            fixture.owner,
            "fixture-scope",
            PilotPermission::SampleSource,
            Uuid::new_v4()
        ),
        Err(PilotAuthorityError::Expired)
    );
    assert!(!fixture.guard(Uuid::new_v4()).reserve_turn_if_allowed(
        &fixture.issuer.claims.thread_id,
        "never-admitted",
        &mut || {}
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn journal_failure_fences_original_allocation_without_refunding_unknown_spend() -> anyhow::Result<()>
{
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::OpenOptionsExt;

    let fixture = Fixture::new();
    fixture.install();
    let home = tempfile::tempdir()?;
    let path = home.path().join("ledger");
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .append(true)
        .mode(0o600)
        .open(&path)?;
    let metadata = file.metadata()?;
    let inputs: Vec<_> = (0..5)
        .map(|_| tempfile::tempfile())
        .collect::<Result<_, _>>()?;
    let journal = Arc::new(PilotCountJournal::receive(
        file,
        PilotLedgerIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
            gid: metadata.gid(),
        },
        [&inputs[0], &inputs[1], &inputs[2], &inputs[3], &inputs[4]],
    )?);
    // Fixture attachment only, in the already installed original ledger. This
    // does not qualify a scope, credential, semantics artifact or count result.
    fixture
        .slot
        .state
        .lock()
        .unwrap()
        .pilot
        .as_mut()
        .unwrap()
        .count_journal = Some(Arc::clone(&journal));
    assert!(fixture.guard(Uuid::new_v4()).reserve_turn_if_allowed(
        &fixture.issuer.claims.thread_id,
        "native-turn",
        &mut || {}
    ));
    let capture = fixture.slot.capture("native-turn")?;
    let mut request = Request::new(
        http::Method::POST,
        "https://fixture.invalid/responses".into(),
    );
    request.headers.insert(
        "x-fixture-context",
        http::HeaderValue::from_static("qualified-fixture"),
    );
    fixture
        .slot
        .begin_attempt_for_request(capture.decision_id, &request)?;
    let mut expected = fixture.slot.pilot_report(fixture.owner)?;
    assert_eq!(expected.reserved_tokens, 1000);

    let mut outside = std::fs::OpenOptions::new().append(true).open(&path)?;
    outside.write_all(b"uncertain external append\n")?;
    let record = journal::CountJournalRecord::Complete(journal::CountComplete {
        version: 1,
        kind: "complete",
        operation: 1,
        decision_id: "fixture".into(),
        attempt_id: "fixture".into(),
        request_id: "fixture".into(),
        input_tokens: "0".into(),
        response_sha256: "a".repeat(/*n*/ 64),
    });
    assert_eq!(journal.append(&record), Err(PilotAuthorityError::Denied));
    expected.revoked = true;
    assert_eq!(fixture.slot.pilot_report(fixture.owner)?, expected);
    assert_eq!(
        fixture.slot.authenticate_pilot_request(&mut request),
        Err(PilotAuthorityError::Expired)
    );
    assert_eq!(
        fixture.slot.check_pilot_grant(
            fixture.owner,
            "fixture-scope",
            PilotPermission::SampleSource,
            Uuid::new_v4()
        ),
        Err(PilotAuthorityError::Expired)
    );
    fixture.clock.lock().unwrap().monotonic += Duration::from_secs(/*secs*/ 2);
    assert!(!fixture.guard(Uuid::new_v4()).reserve_turn_if_allowed(
        &fixture.issuer.claims.thread_id,
        "second-turn",
        &mut || {}
    ));
    assert_eq!(
        fixture
            .slot
            .begin_attempt_for_request(capture.decision_id, &request),
        Err(ObservationError::Unavailable)
    );
    assert_eq!(fixture.slot.pilot_report(fixture.owner)?, expected);
    assert_eq!(std::fs::read(&path)?, b"uncertain external append\n");
    Ok(())
}

#[test]
fn actual_attempts_require_qualification_keep_unknown_spend_and_bind_late_usage() {
    let mut fixture = Fixture::new();
    fixture.install();
    let nonce = Uuid::new_v4();
    assert!(fixture.guard(nonce).reserve_turn_if_allowed(
        &fixture.issuer.claims.thread_id,
        "native-turn",
        &mut || {}
    ));
    let capture = fixture.slot.capture("native-turn").unwrap();
    let mut request = Request::new(
        http::Method::POST,
        "http://fixture.invalid/responses".into(),
    );
    assert_eq!(
        fixture.slot.begin_attempt(capture.decision_id),
        Err(ObservationError::Unavailable)
    );
    assert_eq!(
        fixture
            .slot
            .begin_attempt_for_request(capture.decision_id, &request),
        Err(ObservationError::Unavailable)
    );
    assert_eq!(
        fixture.slot.authenticate_pilot_request(&mut request),
        Ok(codex_client::RequestAuthentication::Provider)
    );
    request
        .headers
        .insert("x-fixture-context", "qualified-fixture".parse().unwrap());
    let first = fixture
        .slot
        .begin_attempt_for_request(capture.decision_id, &request)
        .unwrap();
    let second = fixture
        .slot
        .begin_attempt_for_request(capture.decision_id, &request)
        .unwrap();
    assert_eq!(
        fixture
            .slot
            .begin_attempt_for_request(capture.decision_id, &request),
        Err(ObservationError::Unavailable)
    );
    fixture.slot.revoke().unwrap();
    fixture.slot.release(capture.decision_id).unwrap();
    let mut expected = fixture.slot.pilot_report(fixture.owner).unwrap();
    assert_eq!(expected.attempts.len(), 2);
    for record in &mut expected.attempts {
        record.admission_request_id = Some(nonce);
        record.usage = None;
        record.response_id = None;
        record.response_complete = false;
        record.usage_conflict = false;
    }
    expected.reserved_tokens = 2000;
    expected.active_decision = None;
    expected.revoked = true;
    assert_eq!(fixture.slot.pilot_report(fixture.owner).unwrap(), expected);
    assert_ne!(first, second);
    assert_ne!(
        expected.attempts[0].request_id,
        expected.attempts[1].request_id
    );
    let usage = TokenUsage {
        input_tokens: 20,
        output_tokens: 10,
        total_tokens: 30,
        ..Default::default()
    };
    fixture
        .slot
        .pilot_attempt_completed(first, "response-first", Some(&usage))
        .unwrap();
    fixture
        .slot
        .pilot_attempt_completed(first, "response-first", Some(&usage))
        .unwrap();
    expected.attempts[0].response_id = Some("response-first".into());
    expected.attempts[0].usage = Some(usage);
    expected.attempts[0].response_complete = true;
    assert_eq!(fixture.slot.pilot_report(fixture.owner).unwrap(), expected);
    assert_eq!(
        fixture
            .slot
            .pilot_attempt_completed(first, "conflicting-response", /*usage*/ None),
        Err(PilotAuthorityError::Denied)
    );
    expected.attempts[0].usage_conflict = true;
    assert_eq!(fixture.slot.pilot_report(fixture.owner).unwrap(), expected);
    let mut terminal = Vec::new();
    while let Ok(event) = fixture.events.try_recv() {
        if let ObservationEvent::Submitted(record) = event {
            terminal.push(record.terminal_decision);
        }
    }
    assert_eq!(terminal, vec![false, true]);
}
