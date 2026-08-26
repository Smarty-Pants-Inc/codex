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

No-progress check:
- Classify the previous goal turn as progress, a verified wait, or no progress. Progress changes authoritative state, completes work, or yields evidence that changes the next action; status restatements and unexecuted plans are no progress.
- A verified wait polls a specific process, session, job, or tool handle confirmed live now. Conversation, intent, prior output, or a lock or state file alone is insufficient. Treat work as stopped only when authoritative state says it is terminal or its handle is missing. An observation timeout or transient polling failure is not terminal: re-poll the same handle or inspect other authoritative state; never restart solely because observation expired.
- Revalidate a no-progress turn and take the next available safe action. If none exists because the same genuine blocker remains, report it and leave the goal active until the blocked audit threshold is met. Treat equivalent blockers as the same condition across turns even when their wording or stated next step changes.

Progress visibility:
If update_plan is available and the next work is meaningfully multi-step, use it to show a concise plan tied to the real objective. Keep the plan current as steps complete or the next best action changes. Skip planning overhead for trivial one-step progress, and do not treat a plan update as a substitute for doing the work.

The goal, not the plan, controls continuation. The plan is planning data and may be revised or completed while the goal remains active.

End the goal only by:
- marking it complete when the objective and explicit acceptance criteria are satisfied with direct current evidence; or
- marking it blocked when no meaningful progress is possible without user input or an external-state change, after one reasonable alternate route when one exists.

Do not pause, resume, replace, drop, clear, or rebudget the goal. Those are user or system operations.
