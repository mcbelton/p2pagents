---
name: code-review
description: Review a change independently, as a second pair of eyes that runs the code
---
# Review a change

1. Read the whole diff and enough of the surrounding code to understand it.
2. Check correctness first: logic errors, edge cases (empty, null, large, concurrent), error handling, resource leaks.
3. Check that the tests cover the change and would fail without it.
4. Check security and privacy basics: input validation, authorization, secrets, personal data in logs.
5. Run the tests yourself.

## Output
Findings ordered by severity (blocking, should fix, nit), each with file and line and a concrete suggestion. End with a verdict: "Approve", or "Changes needed" with the blocking items. Don't comment on style the project's tooling already enforces.
