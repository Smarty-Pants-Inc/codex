# CI merge exception policy for agents

GitHub Actions are an important signal, but they are not the sole source of
engineering truth when GitHub-hosted infrastructure is failing.

A pull request may request a human-approved merge exception only when all of the
following are true:

1. No known real PR-introduced failures remain.
2. The focused local validation gate passed.
3. Relevant live smoke/proof passed, if applicable.
4. Normal behavior regressions were checked for the specific area changed.
5. Any remaining GitHub failures are classified as one of:
   - infrastructure/no-runner failure,
   - cancelled/pending with no captured PR-specific failure,
   - pre-existing baseline failure reproduced on clean `origin/main`,
   - external/flaky failure accepted by repo policy.
6. A CI disposition matrix exists.
7. A review bundle exists.
8. A human explicitly authorizes the exception.

## Required CI disposition matrix fields

- PR number and head SHA
- job name
- status/conclusion
- run/job id
- runner metadata if available
- whether logs/steps exist
- classification:
  - PR-passing
  - PR-introduced failure
  - pre-existing failure
  - infra/no-runner failure
  - cancelled/pending
  - unknown
- evidence/log excerpt or note
- action taken
- remaining required action

## Required merge-exception request fields

- PR head SHA
- summary of the change
- local validation commands/results
- live smoke/proof result, if applicable
- list of remaining GitHub failures
- classification of each remaining failure
- baseline comparison for any pre-existing failure claim
- risk assessment
- rollback plan
- review bundle path/hash
- explicit statement: `No known real PR-introduced failures remain.`

## Agent restrictions

Agents must not:

- merge with unknown PR-introduced failures,
- hide failing GitHub jobs,
- edit unrelated baseline failures unless explicitly asked,
- touch unrelated Oracle/Codex areas to satisfy broad lint unless that is the authorized task,
- rerun no-runner jobs indefinitely without new information,
- merge under exception without explicit human approval.

Lightweight rule for this repository: if GitHub is red only because of
documented infra/no-runner, cancelled/no-evidence, or clean-baseline-reproduced
failures, and the focused local gate plus live smoke passed, stop and request
human merge-exception approval instead of spinning.
