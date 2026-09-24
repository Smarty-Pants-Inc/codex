//! Verifies conditional checklist guidance preserves goal instructions and task data.
#![allow(dead_code)]

#[path = "../src/steering.rs"]
mod steering;

use codex_protocol::ThreadId;
use codex_protocol::protocol::ThreadGoal;
use codex_protocol::protocol::ThreadGoalStatus;
use codex_protocol::user_input::UserInput;
use codex_utils_template::Template;
use pretty_assertions::assert_eq;

#[test]
fn enabled_checklist_preserves_the_original_continuation_prompt() {
    let goal = test_goal("Finish the feature.");
    let original = Template::parse(include_str!("../templates/goals/continuation.md"))
        .expect("original continuation template")
        .render([
            ("objective", goal.objective.as_str()),
            ("tokens_used", "100"),
            ("token_budget", "10000"),
            ("remaining_tokens", "9900"),
        ])
        .expect("render original continuation prompt");
    assert_eq!(
        steering::continuation_developer_input(&goal, /*update_plan_enabled*/ true),
        vec![UserInput::Text {
            text: original,
            text_elements: Vec::new(),
        }],
    );
}

#[test]
fn disabled_checklist_preserves_goal_text_that_mentions_the_tool() {
    let objective = "Inspect update_plan.\n\n## Plan tool\nThis is user task data.";
    let input = steering::continuation_developer_input(
        &test_goal(objective),
        /*update_plan_enabled*/ false,
    );
    let [UserInput::Text { text, .. }] = input.as_slice() else {
        panic!("expected goal continuation text");
    };
    assert!(text.contains(objective));
    assert!(!text.contains("If update_plan is available"));
    assert!(text.contains("End the goal only by:"));
}

fn test_goal(objective: &str) -> ThreadGoal {
    ThreadGoal {
        thread_id: ThreadId::new(),
        objective: objective.to_string(),
        status: ThreadGoalStatus::Active,
        token_budget: Some(10_000),
        tokens_used: 100,
        time_used_seconds: 0,
        created_at: 0,
        updated_at: 0,
    }
}
