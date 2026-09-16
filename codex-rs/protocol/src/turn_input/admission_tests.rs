use super::TurnStartAdmission;
use crate::ThreadId;
use crate::turn_input::TurnStartGuard;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::Mutex;

#[derive(Debug, Default)]
struct RecordingAdmission(Mutex<Vec<(ThreadId, String)>>);

impl TurnStartAdmission for RecordingAdmission {
    fn try_commit(&self, thread_id: &ThreadId, turn_id: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .push((*thread_id, turn_id.to_owned()));
        true
    }
}

#[test]
fn clones_commit_one_actual_identity_under_contention() {
    let admission = Arc::new(RecordingAdmission::default());
    let guard = TurnStartGuard::with_admission(admission.clone());
    let thread_id = ThreadId::new();
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| guard.try_commit(&thread_id, "first"));
        let second = scope.spawn(|| guard.clone().try_commit(&thread_id, "second"));
        (first.join().unwrap(), second.join().unwrap())
    });
    let winner = match results {
        (true, false) => "first",
        (false, true) => "second",
        other => panic!("expected exactly one owner admission, got {other:?}"),
    };
    assert_eq!(
        *admission.0.lock().unwrap(),
        vec![(thread_id, winner.to_owned())]
    );
}

#[test]
fn revocation_prevents_owner_commit_through_every_clone() {
    let admission = Arc::new(RecordingAdmission::default());
    let guard = TurnStartGuard::with_admission(admission.clone());
    let sibling = guard.clone();
    guard.revoke();
    assert!(sibling.is_revoked());
    assert!(!sibling.try_commit(&ThreadId::new(), "revoked"));
    assert_eq!(*admission.0.lock().unwrap(), Vec::new());
}

#[test]
fn owner_admission_poison_fails_closed() {
    #[derive(Debug)]
    struct PanickingAdmission;
    impl TurnStartAdmission for PanickingAdmission {
        fn try_commit(&self, _thread_id: &ThreadId, _turn_id: &str) -> bool {
            panic!("synthetic owner failure");
        }
    }
    let guard = TurnStartGuard::with_admission(Arc::new(PanickingAdmission));
    let sibling = guard.clone();
    assert!(
        std::thread::spawn(move || guard.try_commit(&ThreadId::new(), "failed"))
            .join()
            .is_err()
    );
    assert!(sibling.is_revoked());
    assert!(!sibling.try_commit(&ThreadId::new(), "after-failure"));
}
