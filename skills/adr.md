---
name: adr
description: Record an architecture decision as an ADR (status, context, decision, alternatives, consequences)
---
# Architecture decision records

Write one file per significant decision in `design/adr/`, named `ADR-0001-short-title.md` (numbers never reused). A decision is significant if it is hard to reverse, affects other teams, security, privacy, cost or the API, or was argued about. Checked by `design:arch`.

## Format
```markdown
# ADR-0001: Short title in plain words

## Status
Proposed | Accepted | Rejected | Deprecated | Superseded (by ADR-0007)

## Context
The forces at play: the requirement (cite AC or non-functional ids), constraints, what we know and don't know. Neutral: no solution yet.

## Decision
What we will do, in active voice: "We will ...". Specific enough to implement and to check in review.

## Alternatives considered
- **Option**: what it is, why not (cost, risk, fit). At least one real alternative.

## Consequences
- What becomes easier and harder, the risks we accept, follow-up work, what to monitor.
```

## Rules
- One decision per ADR; link related ADRs by id.
- New ADRs start as **Proposed**; the technical review accepts them. Never rewrite an accepted ADR: supersede it with a new one.
- List every ADR under Decisions in `design/architecture.md`.
