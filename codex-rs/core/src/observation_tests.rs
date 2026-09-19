use super::*;
use pretty_assertions::assert_eq;
use std::sync::Barrier;
use std::sync::OnceLock;
use std::sync::Weak;

#[path = "observation_wake_tests.rs"]
mod wake_tests;

#[path = "observation_budget_tests.rs"]
mod budget_tests;

#[path = "observation_capture_tests.rs"]
mod capture_tests;

fn clock() -> ObservationClock {
    ObservationClock {
        wall_seconds: 100,
        monotonic: Instant::now(),
    }
}

fn later(now: ObservationClock, seconds: u64) -> ObservationClock {
    ObservationClock {
        wall_seconds: now.wall_seconds + seconds as i64,
        monotonic: now.monotonic + Duration::from_secs(seconds),
    }
}

fn setup() -> (
    ObservationSlot,
    mpsc::Receiver<ObservationEvent>,
    ObservationOwner,
    Arc<Mutex<ObservationClock>>,
) {
    let time = Arc::new(Mutex::new(clock()));
    let supplied = Arc::clone(&time);
    let (slot, events, owner) = ObservationSlot::with_clock(
        /*connection_id*/ 1,
        Box::new(move || Ok(*supplied.lock().unwrap())),
    );
    (slot, events, owner, time)
}

fn frame(text: &str, expires_at: i64) -> Option<ObservationFrame> {
    Some(ObservationFrame {
        text: Arc::from(text),
        hash: format!("{:x}", Sha256::digest(text.as_bytes())),
        expires_at,
    })
}

#[test]
fn renewal_readback_and_clear_keep_revision_and_fifo_order() {
    let (slot, mut events, owner, time) = setup();
    let now = *time.lock().unwrap();
    let mut expected = slot
        .set(owner, /*revision*/ 1, frame("A", /*expires_at*/ 160))
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Published(expected.clone())
    );
    for elapsed in [30, 60, 90, 120] {
        *time.lock().unwrap() = later(now, elapsed as u64);
        let renewed = slot
            .set(owner, /*revision*/ 1, frame("A", 160 + elapsed))
            .unwrap();
        expected.commit_order += 1;
        expected.expires_at = Some(160 + elapsed);
        assert_eq!(renewed, expected);
        assert_eq!(slot.read(owner).unwrap(), expected);
        assert_eq!(
            events.try_recv().unwrap(),
            ObservationEvent::Published(expected.clone())
        );
        assert_eq!(
            events.try_recv().unwrap(),
            ObservationEvent::Read(expected.clone())
        );
    }
    *time.lock().unwrap() = later(now, /*seconds*/ 121);
    let changed = slot.set(
        owner,
        /*revision*/ 1,
        frame("changed", /*expires_at*/ 281),
    );
    assert_eq!(changed, Err(ObservationError::RevisionMismatch));
    assert_eq!(slot.read(owner).unwrap(), expected);
    let cleared = slot.set(owner, /*revision*/ 2, /*frame*/ None).unwrap();
    assert_eq!(
        cleared,
        ObservationMetadata {
            owner,
            revision: 2,
            commit_order: 6,
            hash: None,
            expires_at: None,
            status: ObservationStatus::Cleared,
            native_reservation: None,
            frame_budget_generation: None,
        }
    );
}

#[test]
fn expiry_is_terminal_even_after_clear_or_a_backwards_wall_clock() {
    let (slot, _events, owner, time) = setup();
    let now = *time.lock().unwrap();
    let body = frame("old success", /*expires_at*/ 160);
    let mut expected = slot.set(owner, /*revision*/ 1, body.clone()).unwrap();
    expected.status = ObservationStatus::Expired;
    *time.lock().unwrap() = ObservationClock {
        monotonic: later(now, /*seconds*/ 60).monotonic,
        ..now
    };
    assert_eq!(slot.read(owner).unwrap(), expected);
    let renewal = slot.set(
        owner,
        /*revision*/ 1,
        frame("old success", /*expires_at*/ 161),
    );
    assert_eq!(renewal, Err(ObservationError::StaleOwner));
    *time.lock().unwrap() = now;
    let replacement = slot.set(owner, /*revision*/ 2, body.clone());
    assert_eq!(replacement, Err(ObservationError::StaleOwner));
    slot.set(owner, /*revision*/ 2, /*frame*/ None).unwrap();
    let replacement = slot.set(owner, /*revision*/ 3, body);
    assert_eq!(replacement, Err(ObservationError::StaleOwner));
}

#[test]
fn forged_owner_and_revoked_owner_cannot_publish_or_read() {
    let (slot, _events, owner, _time) = setup();
    for wrong in [
        ObservationOwner {
            connection_id: 2,
            ..owner
        },
        ObservationOwner {
            epoch: Uuid::new_v4(),
            ..owner
        },
    ] {
        let result = slot.set(wrong, MAX_SEQUENCE, frame("forged", /*expires_at*/ 160));
        assert_eq!(result, Err(ObservationError::StaleOwner));
        assert_eq!(slot.read(wrong), Err(ObservationError::StaleOwner));
    }
    slot.revoke().unwrap();
    let result = slot.set(owner, /*revision*/ 1, frame("new", /*expires_at*/ 160));
    assert_eq!(result, Err(ObservationError::StaleOwner));
    assert_eq!(slot.read(owner), Err(ObservationError::StaleOwner));
}

#[test]
fn invalid_frames_and_queue_or_sequence_overflow_do_not_publish() {
    let (slot, mut events, owner, _time) = setup();
    let mut invalid = frame("A", /*expires_at*/ 160).unwrap();
    invalid.hash = "0".repeat(64);
    let oversized = "x".repeat(MAX_FRAME_BYTES + 1);
    let invalid_frames = [
        Some(invalid),
        frame("\x1b[31m", /*expires_at*/ 160),
        frame(&oversized, /*expires_at*/ 160),
        frame("A", /*expires_at*/ 100),
        frame("A", /*expires_at*/ 161),
    ];
    let initial = slot.read(owner).unwrap();
    for body in invalid_frames {
        assert_eq!(
            slot.set(owner, /*revision*/ 1, body),
            Err(ObservationError::InvalidFrame)
        );
        assert_eq!(slot.read(owner).unwrap(), initial);
    }
    while slot.read(owner).is_ok() {}
    let full = slot.set(owner, /*revision*/ 1, frame("A", /*expires_at*/ 160));
    assert_eq!(full, Err(ObservationError::ResourceLimit));
    while events.try_recv().is_ok() {}
    assert_eq!(slot.read(owner).unwrap(), initial);
    slot.state.lock().unwrap().commit_order = MAX_SEQUENCE;
    let overflow = slot.set(owner, /*revision*/ 1, /*frame*/ None);
    assert_eq!(overflow, Err(ObservationError::ResourceLimit));
}

#[test]
fn unsafe_wire_timestamps_and_capture_ids_are_rejected_before_commit() {
    let (slot, mut events, owner, time) = setup();
    for seconds in [-1, MAX_TIMESTAMP + 1] {
        time.lock().unwrap().wall_seconds = seconds;
        assert_eq!(slot.read(owner), Err(ObservationError::Unavailable));
        assert_eq!(slot.capture("turn"), Err(ObservationError::Unavailable));
        assert_eq!(
            slot.set(owner, /*revision*/ 1, /*frame*/ None),
            Err(ObservationError::Unavailable)
        );
    }
    time.lock().unwrap().wall_seconds = MAX_TIMESTAMP - 1;
    assert_eq!(
        slot.set(owner, /*revision*/ 1, frame("A", MAX_TIMESTAMP + 1)),
        Err(ObservationError::InvalidFrame)
    );
    for turn_id in ["turn\n", "turn\u{7f}"] {
        assert_eq!(slot.capture(turn_id), Err(ObservationError::InvalidFrame));
    }
    assert!(events.try_recv().is_err());
    slot.set(owner, /*revision*/ 1, frame("A", MAX_TIMESTAMP))
        .unwrap();
    let captured = slot.capture("turn").unwrap();
    assert_eq!(captured.captured_at, MAX_TIMESTAMP - 1);
}

#[test]
fn time_is_sampled_under_lock_after_delayed_renewal_and_read() {
    let time = Arc::new(Mutex::new(clock()));
    let supplied = Arc::clone(&time);
    let reference = Arc::new(OnceLock::<Weak<ObservationSlot>>::new());
    let probe = Arc::clone(&reference);
    let (slot, _events, owner) = ObservationSlot::with_clock(
        /*connection_id*/ 1,
        Box::new(move || {
            let slot = probe.get().unwrap().upgrade().unwrap();
            assert!(
                slot.state.try_lock().is_err(),
                "clock must run under the slot lock"
            );
            Ok(*supplied.lock().unwrap())
        }),
    );
    let slot = Arc::new(slot);
    assert!(reference.set(Arc::downgrade(&slot)).is_ok());
    // The uncontended call also detects a supplier incorrectly moved before lock().
    let mut expired = slot
        .set(owner, /*revision*/ 1, frame("A", /*expires_at*/ 160))
        .unwrap();
    expired.status = ObservationStatus::Expired;
    let held = slot.state.lock().unwrap();
    let ready = Barrier::new(/*n*/ 3);
    std::thread::scope(|scope| {
        let renewal = scope.spawn(|| {
            ready.wait();
            slot.set(owner, /*revision*/ 1, frame("A", /*expires_at*/ 220))
        });
        let read = scope.spawn(|| {
            ready.wait();
            slot.read(owner)
        });
        ready.wait();
        let now = *time.lock().unwrap();
        *time.lock().unwrap() = later(now, /*seconds*/ 60);
        drop(held);
        assert_eq!(renewal.join().unwrap(), Err(ObservationError::StaleOwner));
        assert_eq!(read.join().unwrap().unwrap(), expired);
    });
}
