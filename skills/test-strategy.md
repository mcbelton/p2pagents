---
name: test-strategy
description: Write the test plan that proves every acceptance criterion and high-risk threat, at the right level
---
# Test plan

Read `spec/SPEC.md`, `spec/openapi.json`, `design/threat-model.md` and `design/architecture.md`. Write `design/test-plan.md`. Checked by `design:tests`.

## The table

| Covers | Level | Test | Data |
| --- | --- | --- | --- |

- **Covers**: the acceptance criteria (`AC-1`) and threats (`TM-1`) the test proves. Every criterion has a test; every High-risk threat has a test proving its mitigation.
- **Level**: Unit, Integration, Contract, E2E, UI, Manual, Performance, Security or Accessibility.
  - A criterion served by an API operation (its `x-acceptance`) needs a **Contract**, **Integration** or **E2E** test that sends the contract's examples and checks responses against the schemas, status codes and error shapes.
  - Prefer the lowest level that proves it; add one E2E path per story through the real UI.
- **Test**: what is done and what must be observed, specific enough to write the test from.
- **Data**: the example or fixture used (reuse the contract's examples; never real personal data).

## Non-functional testing (a section)
Performance (load profile and the target from the spec), security (the threat tests, dependency and secret scans), accessibility (automated checks and a keyboard and screen reader pass), and anything else the spec requires (reliability, localization).

Each test's name later goes into its criterion's claim in `evidence/claims.json`, so the evidence gate proves the spec.
