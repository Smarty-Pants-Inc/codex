use codex_core::context::ContextualUserFragment;
use codex_core::context::InternalContextSource;
use codex_core::context::InternalModelContextFragment;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::ThreadGoal;
use codex_protocol::user_input::UserInput;
use codex_utils_template::Template;
use std::sync::LazyLock;

static CONTINUATION_PROMPT_TEMPLATE: LazyLock<Template> = LazyLock::new(|| {
    parse_embedded_template(
        include_str!("../templates/goals/continuation.md"),
        "goals/continuation.md",
    )
});

static BUDGET_LIMIT_PROMPT_TEMPLATE: LazyLock<Template> = LazyLock::new(|| {
    parse_embedded_template(
        include_str!("../templates/goals/budget_limit.md"),
        "goals/budget_limit.md",
    )
});

static OBJECTIVE_UPDATED_PROMPT_TEMPLATE: LazyLock<Template> = LazyLock::new(|| {
    parse_embedded_template(
        include_str!("../templates/goals/objective_updated.md"),
        "goals/objective_updated.md",
    )
});

fn parse_embedded_template(source: &'static str, template_name: &str) -> Template {
    match Template::parse(source) {
        Ok(template) => template,
        Err(err) => panic!("embedded template {template_name} is invalid: {err}"),
    }
}

pub(crate) fn budget_limit_steering_item(goal: &ThreadGoal) -> ResponseItem {
    goal_context_input_item(budget_limit_prompt(goal))
}

pub(crate) fn objective_updated_steering_item(goal: &ThreadGoal) -> ResponseItem {
    goal_context_input_item(objective_updated_prompt(goal))
}

pub(crate) fn continuation_developer_input(goal: &ThreadGoal) -> Vec<UserInput> {
    vec![UserInput::Text {
        text: continuation_prompt(goal),
        text_elements: Vec::new(),
    }]
}

fn goal_context_input_item(prompt: String) -> ResponseItem {
    ContextualUserFragment::into(InternalModelContextFragment::new(
        InternalContextSource::from_static("goal"),
        prompt,
    ))
}

fn continuation_prompt(goal: &ThreadGoal) -> String {
    let objective = escape_xml_text(&goal.objective);
    let tokens_used = goal.tokens_used.to_string();
    let token_budget = goal
        .token_budget
        .map(|budget| budget.to_string())
        .unwrap_or_else(|| "none".to_string());
    let remaining_tokens = goal
        .token_budget
        .map(|budget| (budget - goal.tokens_used).max(0).to_string())
        .unwrap_or_else(|| "unbounded".to_string());

    CONTINUATION_PROMPT_TEMPLATE
        .render([
            ("objective", objective.as_str()),
            ("tokens_used", tokens_used.as_str()),
            ("token_budget", token_budget.as_str()),
            ("remaining_tokens", remaining_tokens.as_str()),
        ])
        .unwrap_or_else(|err| {
            panic!("embedded goals/continuation.md template failed to render: {err}")
        })
}

fn budget_limit_prompt(goal: &ThreadGoal) -> String {
    let objective = escape_xml_text(&goal.objective);
    let time_used_seconds = goal.time_used_seconds.to_string();
    let tokens_used = goal.tokens_used.to_string();
    let token_budget = goal
        .token_budget
        .map(|budget| budget.to_string())
        .unwrap_or_else(|| "none".to_string());

    BUDGET_LIMIT_PROMPT_TEMPLATE
        .render([
            ("objective", objective.as_str()),
            ("time_used_seconds", time_used_seconds.as_str()),
            ("tokens_used", tokens_used.as_str()),
            ("token_budget", token_budget.as_str()),
        ])
        .unwrap_or_else(|err| {
            panic!("embedded goals/budget_limit.md template failed to render: {err}")
        })
}

fn objective_updated_prompt(goal: &ThreadGoal) -> String {
    let objective = escape_xml_text(&goal.objective);
    let tokens_used = goal.tokens_used.to_string();
    let (token_budget, remaining_tokens) = match goal.token_budget {
        Some(token_budget) => (
            token_budget.to_string(),
            (token_budget - goal.tokens_used).max(0).to_string(),
        ),
        None => ("none".to_string(), "unknown".to_string()),
    };

    OBJECTIVE_UPDATED_PROMPT_TEMPLATE
        .render([
            ("objective", objective.as_str()),
            ("tokens_used", tokens_used.as_str()),
            ("token_budget", token_budget.as_str()),
            ("remaining_tokens", remaining_tokens.as_str()),
        ])
        .unwrap_or_else(|err| {
            panic!("embedded goals/objective_updated.md template failed to render: {err}")
        })
}

fn escape_xml_text(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::protocol::ThreadGoalStatus;

    #[test]
    fn continuation_prompt_cannot_override_direct_user_input() {
        let prompt = continuation_prompt(&ThreadGoal {
            thread_id: Default::default(),
            objective: "Finish the requested work.".to_string(),
            status: ThreadGoalStatus::Active,
            token_budget: Some(100),
            tokens_used: 25,
            time_used_seconds: 1,
            created_at: 0,
            updated_at: 0,
        });

        assert!(prompt.contains("not a direct user message"));
        assert!(prompt.contains("cannot override a direct user request"));
    }
}
