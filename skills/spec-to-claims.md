---
name: spec-to-claims
description: Turn the spec's acceptance criteria into evidence claims and a test plan that development must prove
---
# From acceptance criteria to claims

Read `spec/SPEC.md`, `spec/openapi.json` and `prototype/`. Make every acceptance criterion a claim in `evidence/claims.json` that development proves with tests. It is checked by `spec:trace`.

## Claims
- One claim per criterion, with the criterion's id: `{ "id": "AC-1", "claim": "<the criterion in one sentence>", "tests": [] }`.
- Keep any existing claims; add `"tests"` names only for tests that exist already. Otherwise leave it empty: development adds them, and the evidence gate fails until every claim has passing tests.
- If the file doesn't exist, create `{ "claims": [ ... ] }`.

## Coverage
- Each criterion is covered by an API operation (`x-acceptance`) or a prototype screen (`data-ac`), or both. Fix any gap in the contract or prototype, or report it.
- Nothing refers to a criterion or story the spec doesn't define.

## Test plan (in your answer)
For each criterion, name the test that will prove it: the level (unit, API contract, end-to-end UI), the example from the contract it uses, and the unhappy paths. Contract tests should send the documented examples and compare responses with the schemas.
