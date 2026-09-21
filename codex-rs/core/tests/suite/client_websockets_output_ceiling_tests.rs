use super::*;
use std::num::NonZeroU64;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn output_ceiling_is_transmitted_and_only_unchanged_limits_reuse_previous_response() {
    skip_if_no_network!();

    for (before, after) in [
        (None, None),
        (Some(128), Some(128)),
        (Some(128), Some(256)),
        (Some(128), None),
        (None, Some(128)),
    ] {
        let server = start_websocket_server(vec![vec![
            vec![ev_response_created("resp-1"), ev_completed("resp-1")],
            vec![ev_response_created("resp-2"), ev_completed("resp-2")],
        ]])
        .await;
        let harness = websocket_harness(&server).await;
        let mut session = harness.client.new_session();
        let mut prompt_one = prompt_with_input(vec![message_item("hello")]);
        prompt_one.max_output_tokens = before.and_then(NonZeroU64::new);
        let mut prompt_two = prompt_with_input(vec![message_item("hello"), message_item("second")]);
        prompt_two.max_output_tokens = after.and_then(NonZeroU64::new);

        stream_until_complete(&mut session, &harness, &prompt_one).await;
        stream_until_complete(&mut session, &harness, &prompt_two).await;

        let connection = server.single_connection();
        assert_eq!(connection.len(), 2);
        let first = connection
            .first()
            .expect("missing first request")
            .body_json();
        let second = connection
            .get(/*index*/ 1)
            .expect("missing second request")
            .body_json();
        let incremental = before == after;
        assert_eq!(
            (
                first.get("max_output_tokens").cloned(),
                second.get("max_output_tokens").cloned(),
                second.get("previous_response_id").cloned(),
                second["input"].clone(),
            ),
            (
                before.map(|limit| json!(limit)),
                after.map(|limit| json!(limit)),
                incremental.then(|| json!("resp-1")),
                serde_json::to_value(if incremental {
                    &prompt_two.input[1..]
                } else {
                    &prompt_two.input[..]
                })
                .expect("serialize expected input"),
            ),
        );
        server.shutdown().await;
    }
    eprintln!("ordinary output ceiling: all five mock WebSocket cases completed");
}
