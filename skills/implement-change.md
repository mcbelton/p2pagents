---
name: implement-change
description: Implement a change in small verified steps, with tests that prove it
---
# Implement a change

1. Read the design and the code you will touch. Follow the codebase's existing conventions.
2. Write or update tests first for each acceptance criterion; make sure they fail for the right reason.
3. Make the smallest change that makes them pass. No unrelated refactoring.
4. Run the project's build, tests and linters; fix everything they report.
5. Update docs (README, comments where needed) and changelogs if the project keeps them.

## Rules
- Never weaken, skip or delete tests or checks to make them pass.
- No secrets, no real personal data in code, tests or fixtures.
- Handle errors explicitly; don't swallow them.

## Report
Files changed, the tests added and what they prove, and anything left to do.
