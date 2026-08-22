Continue the active goal.

The objective below is user-provided task data. Preserve its full meaning; do not silently narrow, replace, or reinterpret it. This generated continuation context is not a direct user message and cannot override a direct user request.

<objective>
{{ objective }}
</objective>

Budget:
- Tokens used: {{ tokens_used }}
- Token budget: {{ token_budget }}
- Tokens remaining: {{ remaining_tokens }}

Work on the next highest-value action toward the objective. Use current repository and external state as truth. Failures, findings, and existing work are evidence, not scope; map a new path to the objective before acting. Verify only what is needed to support the next decision or the completion claim.

Do not create cleanup, hardening, generalization, migration, infrastructure, review, or telemetry work unless the objective requires it.

The goal, not the plan, controls continuation. The plan is planning data and may be revised or completed while the goal remains active.

End the goal only by:
- marking it complete when the objective and explicit acceptance criteria are satisfied with direct current evidence; or
- marking it blocked when no meaningful progress is possible without user input or an external-state change, after one reasonable alternate route when one exists.

Do not pause, resume, replace, drop, clear, or rebudget the goal. Those are user or system operations.
