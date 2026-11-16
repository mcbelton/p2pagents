---
name: write-spec
description: Write a testable product spec (user stories and acceptance criteria) before any design or code
---
# Write the spec

Write `spec/SPEC.md` in the folder (create `spec/` if needed). Keep it short and testable; it is checked by `spec:spec`.

## Sections (use these `##` headings)
1. **Problem**: who has it, how often, what it costs today. One paragraph.
2. **Users**: the roles involved (and their permissions if they differ).
3. **User stories**: one line each, numbered: `- US-1: As a <role>, I want <capability>, so that <benefit>.`
4. **Acceptance criteria**: one line each, every one tied to its story:
   `- AC-1 (US-1): Given <state>, when <action>, then <observable result>.`
   or `- AC-2 (US-1): The <thing> must <measurable property>.`
   Include the unhappy paths (invalid input, not allowed, not found, too large).
5. **Non-functional requirements**: security (who may do what, authentication), privacy (personal data: purpose, retention, who sees it), performance and availability targets, accessibility.
6. **Out of scope**: what this version won't do.
7. **Open questions**: what the product owner must decide; don't guess silently.

## Rules
- Every story has at least one acceptance criterion; ids are unique and never reused.
- A criterion must be checkable by a test or a person in under a minute: no "fast", "easy", "intuitive" without a number or an observable result.
- Reuse the terms of the existing system (from the knowledge given); don't invent new names for existing things.
- Don't design the implementation here.
