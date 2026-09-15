use super::*;
use crate::test_runtime;
use pretty_assertions::assert_eq;
use std::future::Future;
use std::future::poll_fn;
use std::sync::Arc;
use std::task::Poll;
use tokio::sync::oneshot;

#[tokio::test]
async fn stop_completes_before_suspended_on_and_old_intent_cannot_revive() -> anyhow::Result<()> {
    let state = test_runtime().await?;
    let thread_id = ThreadId::new();
    let control = Arc::new(KeepWorking::default());
    control
        .settlement
        .lock()
        .expect("settlement")
        .start("old-turn");
    let intent = control.intent_for_turn("old-turn").expect("running turn");
    let (entered, paused) = oneshot::channel();
    let (release, resume) = oneshot::channel();
    let old_on = tokio::spawn({
        let control = Arc::clone(&control);
        let state = Arc::clone(&state);
        async move {
            entered.send(()).expect("entered receiver");
            resume.await.expect("release old ON");
            control
                .set(
                    state.thread_goals(),
                    thread_id,
                    intent,
                    /*enabled*/ true,
                )
                .await
        }
    });
    paused.await?;
    control
        .stop(state.thread_goals(), thread_id, StopScope::Turn("old-turn"))
        .await
        .expect("stop persists OFF");
    assert!(!state.thread_goals().keep_working_enabled(thread_id).await?);
    assert!(control.halted.load(Ordering::SeqCst));
    release.send(()).expect("old ON receiver");
    assert!(old_on.await?.is_err());
    assert!(!state.thread_goals().keep_working_enabled(thread_id).await?);
    assert!(control.halted.load(Ordering::SeqCst));
    Ok(())
}

#[tokio::test]
async fn stop_revokes_claim_before_waiting_for_writer_and_fresh_on_still_works()
-> anyhow::Result<()> {
    let state = test_runtime().await?;
    let thread_id = ThreadId::new();
    let control = KeepWorking::default();
    control
        .settlement
        .lock()
        .expect("settlement")
        .start("old-turn");
    let intent = control.intent_for_turn("old-turn").expect("running turn");
    control
        .set(
            state.thread_goals(),
            thread_id,
            intent.clone(),
            /*enabled*/ true,
        )
        .await
        .expect("enable before stop");
    let claim = {
        let mut settlement = control.settlement.lock().expect("settlement");
        settlement.finish("old-turn");
        settlement.take_completed().expect("one completed claim")
    };
    let writer = control.writer.acquire().await.expect("writer gate open");
    let mut stopping =
        Box::pin(control.stop(state.thread_goals(), thread_id, control.idle_stop_scope()));
    poll_fn(|cx| {
        assert!(stopping.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(claim.is_revoked());
    assert!(control.halted.load(Ordering::SeqCst));
    drop(writer);
    stopping.await.expect("stop persists OFF");
    assert!(!state.thread_goals().keep_working_enabled(thread_id).await?);

    control
        .settlement
        .lock()
        .expect("settlement")
        .start("fresh-turn");
    let fresh = control
        .intent_for_turn("fresh-turn")
        .expect("fresh running turn");
    control
        .set(
            state.thread_goals(),
            thread_id,
            fresh,
            /*enabled*/ true,
        )
        .await
        .expect("fresh ON after stop");
    // A late old operation remains stale even after genuine new work enabled ON.
    assert!(
        control
            .set(
                state.thread_goals(),
                thread_id,
                intent,
                /*enabled*/ true
            )
            .await
            .is_err()
    );
    assert!(state.thread_goals().keep_working_enabled(thread_id).await?);
    assert!(!control.halted.load(Ordering::SeqCst));
    assert!(claim.is_revoked());
    Ok(())
}

#[tokio::test]
async fn boundaries_revoke_authority_without_restoring_it_from_storage() -> anyhow::Result<()> {
    let state = test_runtime().await?;
    for boundary in [
        ContinuityBoundary::HumanInput(Some("turn")),
        ContinuityBoundary::Resume,
        ContinuityBoundary::Retire,
    ] {
        let thread_id = ThreadId::new();
        let control = KeepWorking::default();
        control.settlement.lock().expect("settlement").start("turn");
        let old = control.intent_for_turn("turn").expect("current intent");
        control
            .set(
                state.thread_goals(),
                thread_id,
                old.clone(),
                /*enabled*/ true,
            )
            .await
            .expect("ON");
        let retired = matches!(boundary, ContinuityBoundary::Retire);
        let stale_idle = control.idle_stop_scope();
        control.invalidate(boundary);
        assert!(old.is_revoked());
        assert!(!control.eligible.load(Ordering::SeqCst));
        assert!(state.thread_goals().keep_working_enabled(thread_id).await?);
        assert!(
            control
                .set(
                    state.thread_goals(),
                    thread_id,
                    old.clone(),
                    /*enabled*/ true
                )
                .await
                .is_err()
        );
        // Exercise the transaction fence itself, not only the caller's early check.
        assert!(
            state
                .thread_goals()
                .set_keep_working_if_current(thread_id, /*enabled*/ false, &old)
                .await
                .is_err()
        );
        assert!(state.thread_goals().keep_working_enabled(thread_id).await?);
        control.invalidate(ContinuityBoundary::HumanInput(Some("current")));
        let fresh = control.intent_for_turn("current");
        if retired {
            assert!(fresh.is_err());
        } else {
            control
                .set(
                    state.thread_goals(),
                    thread_id,
                    fresh.expect("fresh intent"),
                    /*enabled*/ true,
                )
                .await
                .expect("fresh ON");
            // Old turn/idle callbacks cannot stop freshly enabled work, even
            // after the new operation has already completed.
            control
                .stop(state.thread_goals(), thread_id, StopScope::Turn("turn"))
                .await
                .expect("stale turn");
            control
                .settlement
                .lock()
                .expect("settlement")
                .finish("current");
            control
                .stop(state.thread_goals(), thread_id, stale_idle)
                .await
                .expect("stale idle");
        }
        assert_eq!(control.eligible.load(Ordering::SeqCst), !retired);
    }
    Ok(())
}

#[tokio::test]
async fn new_input_revokes_a_stop_waiting_for_the_writer() -> anyhow::Result<()> {
    let state = test_runtime().await?;
    let thread_id = ThreadId::new();
    let control = KeepWorking::default();
    control.settlement.lock().expect("settlement").start("old");
    let old = control.intent_for_turn("old").expect("current intent");
    control
        .set(state.thread_goals(), thread_id, old, /*enabled*/ true)
        .await
        .expect("ON");
    let writer = control.writer.acquire().await.expect("writer");
    let mut stopping =
        Box::pin(control.stop(state.thread_goals(), thread_id, StopScope::Turn("old")));
    poll_fn(|cx| {
        assert!(stopping.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    control.invalidate(ContinuityBoundary::HumanInput(Some("current")));
    drop(writer);
    assert!(stopping.await.is_err());
    assert!(state.thread_goals().keep_working_enabled(thread_id).await?);
    assert!(!control.eligible.load(Ordering::SeqCst));
    Ok(())
}
