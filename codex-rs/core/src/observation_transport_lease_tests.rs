use super::*;
use futures::poll;

#[tokio::test]
async fn every_original_callback_lease_must_drop_before_native_drain() {
    let (slot, _events, _) = ObservationSlot::new(/*connection_id*/ 7);
    let slot = Arc::new(slot);
    let transport = Arc::clone(&slot).transport_lease();
    let callback = Arc::clone(&transport);
    slot.revoke().unwrap();
    let first = slot.wait_for_transport_drain();
    let second = slot.wait_for_transport_drain();
    tokio::pin!(first, second);
    assert!(poll!(first.as_mut()).is_pending());
    assert!(poll!(second.as_mut()).is_pending());
    drop(transport);
    assert!(poll!(first.as_mut()).is_pending());
    drop(callback);
    assert!(poll!(first.as_mut()).is_ready());
    assert!(poll!(second.as_mut()).is_ready());
}
