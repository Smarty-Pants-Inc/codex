use super::*;
use crate::ObservationProfile;
use crate::ObservationReservationState;
use crate::ObservationSlot;
use codex_protocol::protocol::AskForApproval;
use pretty_assertions::assert_eq;
use std::future::Future;
use std::sync::Arc;
use std::task::Context;
use std::task::Waker;

async fn setup() -> (
    Session,
    Arc<ObservationSlot>,
    tokio::sync::mpsc::Receiver<crate::ObservationEvent>,
    ObservationOwner,
) {
    let (session, _) = crate::session::tests::make_session_and_context().await;
    {
        let mut state = session.state.lock().await;
        let configuration = &mut state.session_configuration;
        configuration.collaboration_mode = configuration.collaboration_mode.with_updates(
            Some("gpt-oss-20b".into()),
            /*effort*/ None,
            /*developer_instructions*/ None,
        );
        let mut config = (*configuration.original_config_do_not_use).clone();
        config.model_context_window = Some(131_072);
        configuration.original_config_do_not_use = Arc::new(config);
    }
    let (slot, events, owner) = ObservationSlot::new(/*connection_id*/ 1);
    let slot = Arc::new(slot);
    session
        .install_budgeted_observation_binding(
            ObservationBinding {
                slot: Arc::clone(&slot),
                profile: ObservationProfile::HarmonyGptOss,
            },
            owner,
        )
        .await
        .unwrap();
    (session, slot, events, owner)
}

#[tokio::test]
async fn owner_read_cannot_revalidate_old_configuration_inside_settings_commit_gap() {
    let (session, slot, _events, owner) = setup().await;
    let mut state = session.state.lock().await;
    session.invalidate_observation_budget();
    let invalid = slot.native_reservation(owner).unwrap();
    let mut read = Box::pin(session.revalidate_observation_budget(owner));
    assert!(
        read.as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    assert_eq!(slot.native_reservation(owner).unwrap(), invalid);
    state.session_configuration.collaboration_mode =
        state.session_configuration.collaboration_mode.with_updates(
            Some("gpt-5".into()),
            /*effort*/ None,
            /*developer_instructions*/ None,
        );
    drop(state);
    read.await.unwrap();
    let mut expected = invalid;
    expected.generation += 1;
    expected.model = Some("gpt-5".into());
    expected.state = ObservationReservationState::Unsupported;
    // Effective context percent is model-specific; the point here is that the
    // joined snapshot names NEW settings, not the old configuration in the gap.
    let actual = slot.native_reservation(owner).unwrap();
    expected.usable_context_tokens = actual.usable_context_tokens;
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn failed_settings_update_preserves_budget_and_successful_aba_cannot_restore_old_generation()
{
    let (session, slot, _events, owner) = setup().await;
    let initial = slot.native_reservation(owner).unwrap();
    let mode = {
        let mut state = session.state.lock().await;
        state.session_configuration.approval_policy =
            codex_config::Constrained::allow_only(AskForApproval::Never);
        state.session_configuration.collaboration_mode.clone()
    };
    let update = super::super::SessionSettingsUpdate {
        collaboration_mode: Some(mode.with_updates(
            Some("gpt-5".into()),
            /*effort*/ None,
            /*developer_instructions*/ None,
        )),
        approval_policy: Some(AskForApproval::OnRequest),
        ..Default::default()
    };
    assert!(session.update_settings(update).await.is_err());
    assert_eq!(slot.native_reservation(owner).unwrap(), initial);
    for model in ["gpt-5", "gpt-oss-20b"] {
        session
            .update_settings(super::super::SessionSettingsUpdate {
                collaboration_mode: Some(mode.with_updates(
                    Some(model.into()),
                    /*effort*/ None,
                    /*developer_instructions*/ None,
                )),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            slot.native_reservation(owner).unwrap().state,
            ObservationReservationState::Invalid
        );
        session.revalidate_observation_budget(owner).await.unwrap();
    }
    let mut expected = initial;
    expected.generation += 4;
    assert_eq!(slot.native_reservation(owner).unwrap(), expected);
}
