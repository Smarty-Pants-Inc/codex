//! Integration tests that cover compacting, resuming, and forking conversations.
//!
//! Each test sets up a mocked SSE conversation and drives the conversation through
//! a specific sequence of operations. After every operation we capture the
//! request payload that Codex would send to the model and assert that the
//! model-visible history matches the expected sequence of messages.

use super::compact::COMPACT_WARNING_MESSAGE;
use super::compact::FIRST_REPLY;
use super::compact::SUMMARY_TEXT;
use anyhow::Context;
use anyhow::Result;
use codex_core::CodexThread;
use codex_core::ThreadManager;
use codex_core::TurnInputRequest;
use codex_core::compact::SUMMARIZATION_PROMPT;
use codex_core::config::Config;
use codex_core::spawn::CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR;
use codex_history::CodexHarnessMetadata;
use codex_history::RolloutItem;
use codex_history::RolloutLine;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::mcp::ClientMcpExtensions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::protocol::WarningEvent;
use codex_protocol::user_input::UserInput;
use core_test_support::context_snapshot;
use core_test_support::context_snapshot::ContextSnapshotOptions;
use core_test_support::context_snapshot::ContextSnapshotRenderMode;
use core_test_support::responses::ResponseMock;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::test_codex::local_selections;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use wiremock::MockServer;

const AFTER_SECOND_RESUME: &str = "AFTER_SECOND_RESUME";
const AFTER_ROLLBACK: &str = "AFTER_ROLLBACK";
const CHECKPOINT_METADATA_KEY: &str = "replacement_history_metadata";

fn network_disabled() -> bool {
    std::env::var(CODEX_SANDBOX_NETWORK_DISABLED_ENV_VAR).is_ok()
}

fn body_contains_text(body: &str, text: &str) -> bool {
    body.contains(&json_fragment(text))
}

fn json_fragment(text: &str) -> String {
    serde_json::to_string(text)
        .expect("serialize text to JSON")
        .trim_matches('"')
        .to_string()
}

fn normalize_line_endings_str(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_string()
    }
}

fn response_message_contains_text(item: &ResponseItem, expected: &str) -> bool {
    matches!(
        item,
        ResponseItem::Message { role, content, .. }
            if role == "user"
                && content.iter().any(|item| {
                    matches!(item, ContentItem::InputText { text } if text == expected)
                })
    )
}

fn seed_first_checkpoint_harness_metadata(path: &Path, retained_text: &str) -> Result<()> {
    let mut lines = std::fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<RolloutLine>)
        .collect::<Result<Vec<_>, _>>()?;
    let replacement_history = lines
        .iter_mut()
        .find_map(|line| match &mut line.item {
            RolloutItem::Compacted(compacted) => compacted.replacement_history.as_mut(),
            _ => None,
        })
        .context("first compacted checkpoint missing replacement history")?;
    replacement_history
        .iter()
        .find(|envelope| response_message_contains_text(&envelope.item, retained_text))
        .context("retained user message missing from first compacted checkpoint")?;
    for envelope in replacement_history {
        envelope.metadata = Some(CodexHarnessMetadata::default());
    }

    let rewritten = lines
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    std::fs::write(path, format!("{rewritten}\n"))?;
    Ok(())
}

fn assert_latest_checkpoint_retains_harness_metadata(
    path: &Path,
    retained_text: &str,
) -> Result<()> {
    let replacement_history = std::fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<RolloutLine>)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .rev()
        .find_map(|line| match line.item {
            RolloutItem::Compacted(compacted) => compacted.replacement_history,
            _ => None,
        })
        .context("latest compacted checkpoint missing replacement history")?;
    assert!(
        replacement_history.iter().any(|envelope| {
            response_message_contains_text(&envelope.item, retained_text)
                && envelope.metadata.is_some()
        }),
        "latest compacted checkpoint should retain aligned harness metadata on {retained_text:?}"
    );
    Ok(())
}

fn extract_summary_text(request: &Value, summary_text: &str) -> String {
    json_message_input_texts(request, "developer")
        .into_iter()
        .find(|text| text.contains(summary_text))
        .expect("expected developer summary message")
}

fn json_message_input_texts(request: &Value, role: &str) -> Vec<String> {
    request
        .get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("message")
                && item.get("role").and_then(Value::as_str) == Some(role)
        })
        .filter_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .and_then(|content| content.first())
                .and_then(|entry| entry.get("text"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn normalize_compact_prompts(requests: &mut [Value]) {
    let normalized_summary_prompt = normalize_line_endings_str(SUMMARIZATION_PROMPT);
    for request in requests {
        if let Some(input) = request.get_mut("input").and_then(Value::as_array_mut) {
            input.retain(|item| {
                if item.get("type").and_then(Value::as_str) != Some("message") {
                    return true;
                }
                let Some(content) = item.get("content").and_then(Value::as_array) else {
                    return false;
                };
                let Some(first) = content.first() else {
                    return false;
                };
                let text = first
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let normalized_text = normalize_line_endings_str(text);
                !(text.is_empty() || normalized_text == normalized_summary_prompt)
            });
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// Scenario: compact an initial conversation, resume it, fork one turn back, and
/// ensure the model-visible history matches expectations at each request.
async fn compact_resume_and_fork_preserve_model_history_view() {
    if network_disabled() {
        println!("Skipping test because network is disabled in this sandbox");
        return;
    }

    // 1. Arrange mocked SSE responses for the initial compact/resume/fork flow.
    let server = MockServer::start().await;
    let request_log = mount_initial_flow(&server).await;
    let expected_model = "gpt-5.4";
    // 2. Start a new conversation and drive it through the compact/resume/fork steps.
    let (_home, config, manager, base) =
        start_test_conversation(&server, Some(expected_model)).await;

    user_turn(&base, "hello world").await;
    compact_conversation(&base).await;
    user_turn(&base, "AFTER_COMPACT").await;
    let base_path = fetch_conversation_path(&base);
    assert!(
        base_path.exists(),
        "compact+resume test expects base path {base_path:?} to exist",
    );

    shutdown_conversation(&base).await;
    let resumed = resume_conversation(&manager, &config, base_path).await;
    user_turn(&resumed, "AFTER_RESUME").await;
    let resumed_path = fetch_conversation_path(&resumed);
    assert!(
        resumed_path.exists(),
        "compact+resume test expects resumed path {resumed_path:?} to exist",
    );

    let forked = fork_thread(&manager, &config, resumed_path, /*nth_user_message*/ 2).await;
    user_turn(&forked, "AFTER_FORK").await;

    // 3. Capture the requests to the model and validate the history slices.
    let mut requests = gather_request_bodies(&request_log);
    normalize_compact_prompts(&mut requests);

    // input after compact is a prefix of input after resume/fork
    let input_after_compact = json!(requests[requests.len() - 3]["input"]);
    let input_after_resume = json!(requests[requests.len() - 2]["input"]);
    let input_after_fork = json!(requests[requests.len() - 1]["input"]);

    let compact_arr = input_after_compact
        .as_array()
        .expect("input after compact should be an array");
    let resume_arr = input_after_resume
        .as_array()
        .expect("input after resume should be an array");
    let fork_arr = input_after_fork
        .as_array()
        .expect("input after fork should be an array");

    assert!(
        compact_arr.len() <= resume_arr.len(),
        "after-resume input should have at least as many items as after-compact",
    );
    assert_eq!(compact_arr.as_slice(), &resume_arr[..compact_arr.len()]);

    assert!(
        compact_arr.len() <= fork_arr.len(),
        "after-fork input should have at least as many items as after-compact",
    );
    assert_eq!(
        &compact_arr.as_slice()[..compact_arr.len()],
        &fork_arr[..compact_arr.len()]
    );

    assert_eq!(
        json_message_input_texts(&requests[0], "user"),
        vec!["hello world"]
    );
    extract_summary_text(&requests[2], SUMMARY_TEXT);
    extract_summary_text(&requests[3], SUMMARY_TEXT);
    extract_summary_text(&requests[4], SUMMARY_TEXT);
    assert_eq!(
        json_message_input_texts(&requests[2], "user"),
        vec!["hello world", "AFTER_COMPACT"]
    );
    assert_eq!(
        json_message_input_texts(&requests[3], "user"),
        vec!["hello world", "AFTER_COMPACT", "AFTER_RESUME"]
    );
    assert_eq!(
        json_message_input_texts(&requests[4], "user"),
        vec!["hello world", "AFTER_COMPACT", "AFTER_FORK"]
    );
    assert_eq!(requests.len(), 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// Scenario: after the forked branch is compacted, resuming again should reuse
/// the compacted history and only append the new user message.
async fn compact_resume_after_second_compaction_preserves_history() -> Result<()> {
    if network_disabled() {
        println!("Skipping test because network is disabled in this sandbox");
        return Ok(());
    }

    // 1. Arrange mocked SSE responses as a single ordered stream so assertions
    // observe the real request sequence instead of per-mock duplicate captures.
    let server = MockServer::start().await;
    let request_log = mount_second_compact_sequence(&server).await;

    // 2. Drive the conversation through compact -> resume -> fork -> compact -> resume.
    let (_home, config, manager, base) = start_test_conversation(&server, /*model*/ None).await;

    user_turn(&base, "hello world").await;
    compact_conversation(&base).await;
    user_turn(&base, "AFTER_COMPACT").await;
    let base_path = fetch_conversation_path(&base);
    assert!(
        base_path.exists(),
        "second compact test expects base path {base_path:?} to exist",
    );

    shutdown_conversation(&base).await;
    seed_first_checkpoint_harness_metadata(&base_path, "hello world")?;
    let resumed = resume_conversation(&manager, &config, base_path).await;
    user_turn(&resumed, "AFTER_RESUME").await;
    let resumed_path = fetch_conversation_path(&resumed);
    assert!(
        resumed_path.exists(),
        "second compact test expects resumed path {resumed_path:?} to exist",
    );

    let forked = fork_thread(&manager, &config, resumed_path, /*nth_user_message*/ 3).await;
    user_turn(&forked, "AFTER_FORK").await;

    compact_conversation(&forked).await;
    user_turn(&forked, "AFTER_COMPACT_2").await;
    let forked_path = fetch_conversation_path(&forked);
    assert!(
        forked_path.exists(),
        "second compact test expects forked path {forked_path:?} to exist",
    );

    shutdown_conversation(&forked).await;
    assert_latest_checkpoint_retains_harness_metadata(&forked_path, "hello world")?;
    let resumed_again = resume_conversation(&manager, &config, forked_path).await;
    user_turn(&resumed_again, AFTER_SECOND_RESUME).await;

    let mut requests = request_log
        .requests()
        .into_iter()
        .map(|request| request.body_json())
        .collect::<Vec<_>>();
    requests.iter_mut().for_each(normalize_line_endings);
    normalize_compact_prompts(&mut requests);
    assert!(
        requests
            .iter()
            .flat_map(|request| request["input"].as_array().expect("provider request input"))
            .all(|item| {
                item.get(CHECKPOINT_METADATA_KEY).is_none() && item.get("metadata").is_none()
            }),
        "provider requests must not contain harness metadata"
    );
    let input_after_compact = json!(requests[requests.len() - 2]["input"]);
    let input_after_resume = json!(requests[requests.len() - 1]["input"]);

    // test input after compact before resume is the same as input after resume
    let compact_input_array = input_after_compact
        .as_array()
        .expect("input after compact should be an array");
    let resume_input_array = input_after_resume
        .as_array()
        .expect("input after resume should be an array");
    assert!(
        compact_input_array.len() <= resume_input_array.len(),
        "after-resume input should have at least as many items as after-compact"
    );
    assert_eq!(
        compact_input_array.as_slice(),
        &resume_input_array[..compact_input_array.len()]
    );
    assert_eq!(
        json_message_input_texts(&requests[0], "user"),
        vec!["hello world"]
    );
    extract_summary_text(&requests[requests.len() - 2], SUMMARY_TEXT);
    let final_user_texts = json_message_input_texts(&requests[requests.len() - 1], "user");
    let expected_full_history = vec![
        "hello world",
        "AFTER_COMPACT",
        "AFTER_RESUME",
        "AFTER_FORK",
        "AFTER_COMPACT_2",
        AFTER_SECOND_RESUME,
    ];
    let expected_fork_local_history = vec!["AFTER_FORK", "AFTER_COMPACT_2", AFTER_SECOND_RESUME];
    assert!(
        final_user_texts == expected_full_history
            || final_user_texts == expected_fork_local_history,
        "unexpected after-second-resume user history: {final_user_texts:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// Scenario: rolling back behind a pre-turn compaction should replay
/// append-only history from the rollout file and keep earlier compacted
/// history visible.
async fn snapshot_rollback_past_compaction_replays_append_only_history() -> Result<()> {
    if network_disabled() {
        println!("Skipping test because network is disabled in this sandbox");
        return Ok(());
    }

    const EDITED_AFTER_COMPACT: &str = "EDITED_AFTER_COMPACT";
    const SECOND_REPLY: &str = "SECOND_REPLY";

    let server = MockServer::start().await;
    let sse1 = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed("r1"),
    ]);
    let sse2 = sse(vec![
        ev_assistant_message("m2", SUMMARY_TEXT),
        ev_completed("r2"),
    ]);
    let sse3 = sse(vec![
        ev_assistant_message("m3", SECOND_REPLY),
        ev_completed("r3"),
    ]);
    let sse4 = sse(vec![ev_completed("r4")]);

    let request_log = mount_sse_sequence(&server, vec![sse1, sse2, sse3, sse4]).await;

    let (_home, _config, _manager, base) = start_test_conversation(&server, /*model*/ None).await;

    user_turn(&base, "hello world").await;
    compact_conversation(&base).await;
    user_turn(&base, EDITED_AFTER_COMPACT).await;

    base.submit(Op::ThreadRollback { num_turns: 1 })
        .await
        .expect("submit thread rollback");
    let rollback_event =
        wait_for_event(&base, |ev| matches!(ev, EventMsg::ThreadRolledBack(_))).await;
    let EventMsg::ThreadRolledBack(rollback_event) = rollback_event else {
        panic!("expected thread rolled back event");
    };
    assert_eq!(rollback_event.num_turns, 1);

    user_turn(&base, AFTER_ROLLBACK).await;

    let requests = request_log.requests();
    assert_eq!(requests.len(), 4);
    assert!(requests[1].body_contains_text(SUMMARIZATION_PROMPT));
    assert!(requests[2].body_contains_text("hello world"));
    assert!(requests[2].body_contains_text(SUMMARY_TEXT));
    assert!(requests[2].body_contains_text(EDITED_AFTER_COMPACT));
    assert_eq!(
        requests[3].message_input_texts("user"),
        vec!["hello world", AFTER_ROLLBACK]
    );
    assert!(
        requests[3].body_contains_text("hello world"),
        "the first turn should remain visible after rollback behind compaction",
    );
    assert!(
        !requests[3].body_contains_text(EDITED_AFTER_COMPACT),
        "the edited post-compaction turn should be removed by rollback",
    );
    assert!(
        requests[3].body_contains_text(SUMMARY_TEXT),
        "compaction summary should remain for the preserved first turn",
    );

    insta::assert_snapshot!(
        "rollback_past_compaction_shapes",
        context_snapshot::format_labeled_requests_snapshot(
            "rollback past compaction replay after rollback",
            &[
                ("compaction request", &requests[1]),
                ("before rollback", &requests[2]),
                ("after rollback", &requests[3]),
            ],
            &ContextSnapshotOptions::default()
                .strip_capability_instructions()
                .render_mode(ContextSnapshotRenderMode::KindWithTextPrefix { max_chars: 64 }),
        )
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// Scenario: rolling back a turn that introduced persistent pre-thread settings
/// diffs should trim those context updates so the next request includes them
/// only once.
async fn snapshot_rollback_followup_turn_trims_context_updates() -> Result<()> {
    if network_disabled() {
        println!("Skipping test because network is disabled in this sandbox");
        return Ok(());
    }

    const MODEL: &str = "gpt-5.4";
    const TURN_ONE_USER: &str = "turn 1 user";
    const TURN_TWO_USER: &str = "turn 2 user";
    const FOLLOWUP_USER: &str = "follow-up user";
    const ROLLED_BACK_DEV_INSTRUCTIONS: &str = "ROLLED_BACK_DEV_INSTRUCTIONS";
    const PRETURN_CONTEXT_DIFF_CWD: &str = "PRETURN_CONTEXT_DIFF_CWD";

    let server = MockServer::start().await;
    let request_log = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_assistant_message("m1", "turn 1 assistant"),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_assistant_message("m2", "turn 2 assistant"),
                ev_completed("r2"),
            ]),
            sse(vec![ev_response_created("r3"), ev_completed("r3")]),
        ],
    )
    .await;

    let (_home, config, _manager, conversation) =
        start_test_conversation(&server, Some(MODEL)).await;

    user_turn(&conversation, TURN_ONE_USER).await;

    let override_cwd = config.cwd.join(PRETURN_CONTEXT_DIFF_CWD);
    std::fs::create_dir_all(&override_cwd)?;
    core_test_support::submit_thread_settings(
        &conversation,
        ThreadSettingsOverrides {
            environments: Some(local_selections(override_cwd.clone())),
            collaboration_mode: Some(CollaborationMode {
                mode: ModeKind::Default,
                settings: Settings {
                    model: MODEL.to_string(),
                    reasoning_effort: None,
                    developer_instructions: Some(ROLLED_BACK_DEV_INSTRUCTIONS.to_string()),
                },
            }),
            ..Default::default()
        },
    )
    .await?;

    user_turn(&conversation, TURN_TWO_USER).await;

    conversation
        .submit(Op::ThreadRollback { num_turns: 1 })
        .await?;
    let rollback_event = wait_for_event(&conversation, |ev| {
        matches!(ev, EventMsg::ThreadRolledBack(_))
    })
    .await;
    let EventMsg::ThreadRolledBack(rollback_event) = rollback_event else {
        panic!("expected thread rolled back event");
    };
    assert_eq!(rollback_event.num_turns, 1);

    user_turn(&conversation, FOLLOWUP_USER).await;

    let requests = request_log.requests();
    assert_eq!(requests.len(), 3);

    let before_rollback_developer_count = requests[1]
        .message_input_texts("developer")
        .iter()
        .filter(|text| text.contains(ROLLED_BACK_DEV_INSTRUCTIONS))
        .count();
    assert_eq!(before_rollback_developer_count, 1);
    assert_eq!(
        requests[1]
            .message_input_texts("developer")
            .iter()
            .filter(|text| text.contains(PRETURN_CONTEXT_DIFF_CWD))
            .count(),
        1
    );
    assert_eq!(
        requests[1].message_input_texts("user"),
        vec![TURN_ONE_USER, TURN_TWO_USER]
    );

    let after_rollback_developer_count = requests[2]
        .message_input_texts("developer")
        .iter()
        .filter(|text| text.contains(ROLLED_BACK_DEV_INSTRUCTIONS))
        .count();
    assert_eq!(after_rollback_developer_count, 1);

    let after_rollback_developer_texts = requests[2].message_input_texts("developer");
    assert_eq!(
        after_rollback_developer_texts
            .iter()
            .filter(|text| text.contains(PRETURN_CONTEXT_DIFF_CWD))
            .count(),
        1
    );
    assert_eq!(
        requests[2].message_input_texts("user"),
        vec![TURN_ONE_USER, FOLLOWUP_USER]
    );

    insta::assert_snapshot!(
        "rollback_followup_turn_trims_context_updates",
        context_snapshot::format_labeled_requests_snapshot(
            "rollback trims pre-turn override context updates before the follow-up request",
            &[
                ("rolled-back turn request", &requests[1]),
                ("follow-up request after rollback", &requests[2]),
            ],
            &ContextSnapshotOptions::default()
                .strip_capability_instructions()
                .render_mode(ContextSnapshotRenderMode::KindWithTextPrefix { max_chars: 96 }),
        )
    );

    Ok(())
}

fn normalize_line_endings(value: &mut Value) {
    match value {
        Value::String(text) if text.contains('\r') => {
            *text = text.replace("\r\n", "\n").replace('\r', "\n");
        }
        Value::Array(items) => {
            for item in items {
                normalize_line_endings(item);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                normalize_line_endings(item);
            }
        }
        _ => {}
    }
}

fn gather_requests(request_log: &[ResponseMock]) -> Vec<ResponsesRequest> {
    request_log
        .iter()
        .flat_map(ResponseMock::requests)
        .collect::<Vec<_>>()
}

fn gather_request_bodies(request_log: &[ResponseMock]) -> Vec<Value> {
    let mut bodies = gather_requests(request_log)
        .into_iter()
        .map(|request| request.body_json())
        .collect::<Vec<_>>();
    bodies.iter_mut().for_each(normalize_line_endings);
    bodies
}

async fn mount_initial_flow(server: &MockServer) -> Vec<ResponseMock> {
    let sse1 = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed("r1"),
    ]);
    let sse2 = sse(vec![
        ev_assistant_message("m2", SUMMARY_TEXT),
        ev_completed("r2"),
    ]);
    let sse3 = sse(vec![
        ev_assistant_message("m3", "AFTER_COMPACT_REPLY"),
        ev_completed("r3"),
    ]);
    let sse4 = sse(vec![ev_completed("r4")]);
    let sse5 = sse(vec![ev_completed("r5")]);

    let match_first = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body.contains("\"text\":\"hello world\"")
            && !body.contains(&format!("\"text\":\"{SUMMARY_TEXT}\""))
            && !body.contains("\"text\":\"AFTER_COMPACT\"")
            && !body.contains("\"text\":\"AFTER_RESUME\"")
            && !body.contains("\"text\":\"AFTER_FORK\"")
    };
    let first = mount_sse_once_match(server, match_first, sse1).await;

    let match_compact = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body_contains_text(body, SUMMARIZATION_PROMPT) || body.contains(&json_fragment(FIRST_REPLY))
    };
    let compact = mount_sse_once_match(server, match_compact, sse2).await;

    let match_after_compact = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body.contains("\"text\":\"AFTER_COMPACT\"")
            && !body.contains("\"text\":\"AFTER_RESUME\"")
            && !body.contains("\"text\":\"AFTER_FORK\"")
    };
    let after_compact = mount_sse_once_match(server, match_after_compact, sse3).await;

    let match_after_resume = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body.contains("\"text\":\"AFTER_RESUME\"")
    };
    let after_resume = mount_sse_once_match(server, match_after_resume, sse4).await;

    let match_after_fork = |req: &wiremock::Request| {
        let body = std::str::from_utf8(&req.body).unwrap_or("");
        body.contains("\"text\":\"AFTER_FORK\"")
    };
    let after_fork = mount_sse_once_match(server, match_after_fork, sse5).await;

    vec![first, compact, after_compact, after_resume, after_fork]
}

async fn mount_second_compact_sequence(server: &MockServer) -> ResponseMock {
    let sse1 = sse(vec![
        ev_assistant_message("m1", FIRST_REPLY),
        ev_completed("r1"),
    ]);
    let sse2 = sse(vec![
        ev_assistant_message("m2", SUMMARY_TEXT),
        ev_completed("r2"),
    ]);
    let sse3 = sse(vec![
        ev_assistant_message("m3", "AFTER_COMPACT_REPLY"),
        ev_completed("r3"),
    ]);
    let sse4 = sse(vec![ev_completed("r4")]);
    let sse5 = sse(vec![ev_completed("r5")]);
    let sse6 = sse(vec![
        ev_assistant_message("m4", SUMMARY_TEXT),
        ev_completed("r6"),
    ]);
    let sse7 = sse(vec![ev_completed("r7")]);
    let sse8 = sse(vec![ev_completed("r8")]);

    mount_sse_sequence(server, vec![sse1, sse2, sse3, sse4, sse5, sse6, sse7, sse8]).await
}

async fn start_test_conversation(
    server: &MockServer,
    model: Option<&str>,
) -> (Arc<TempDir>, Config, Arc<ThreadManager>, Arc<CodexThread>) {
    let base_url = format!("{}/v1", server.uri());
    let model = model.map(str::to_string);
    let mut builder = test_codex().with_config(move |config| {
        config.model_provider.name = "Non-OpenAI Model provider".to_string();
        config.model_provider.base_url = Some(base_url);
        config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
        if let Some(model) = model {
            config.model = Some(model);
        }
    });
    let test = Box::pin(builder.build(server))
        .await
        .expect("create conversation");
    (test.home, test.config, test.thread_manager, test.codex)
}

async fn user_turn(conversation: &Arc<CodexThread>, text: &str) {
    conversation
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: text.into(),
            text_elements: Vec::new(),
        }]))
        .await
        .expect("submit user turn");
    wait_for_event(conversation, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
}

async fn compact_conversation(conversation: &Arc<CodexThread>) {
    conversation
        .submit(Op::Compact)
        .await
        .expect("compact conversation");
    let warning_event = wait_for_event(conversation, |ev| {
        matches!(
            ev,
            EventMsg::Warning(WarningEvent { message }) if message == COMPACT_WARNING_MESSAGE
        )
    })
    .await;
    let EventMsg::Warning(WarningEvent { message }) = warning_event else {
        panic!("expected warning event after compact");
    };
    assert_eq!(message, COMPACT_WARNING_MESSAGE);
    wait_for_event(conversation, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
}

fn fetch_conversation_path(conversation: &Arc<CodexThread>) -> std::path::PathBuf {
    conversation.rollout_path().expect("rollout path")
}

async fn shutdown_conversation(conversation: &Arc<CodexThread>) {
    conversation
        .shutdown_and_wait()
        .await
        .expect("shutdown conversation");
}

async fn resume_conversation(
    manager: &ThreadManager,
    config: &Config,
    path: std::path::PathBuf,
) -> Arc<CodexThread> {
    let auth_manager = codex_core::test_support::auth_manager_from_auth(
        codex_login::CodexAuth::from_api_key("dummy"),
    );
    Box::pin(manager.resume_thread_from_rollout(
        config.clone(),
        path,
        auth_manager,
        /*parent_trace*/ None,
        ClientMcpExtensions::default(),
    ))
    .await
    .expect("resume conversation")
    .thread
}

#[cfg(test)]
async fn fork_thread(
    manager: &ThreadManager,
    config: &Config,
    path: std::path::PathBuf,
    nth_user_message: usize,
) -> Arc<CodexThread> {
    Box::pin(manager.fork_thread(
        nth_user_message,
        config.clone(),
        path,
        /*thread_source*/ None,
        /*parent_trace*/ None,
    ))
    .await
    .expect("fork conversation")
    .thread
}
