---
marp: true
paginate: true
---

# {{title}}
## Technical review

Architecture, decisions, API, data, threats, tests and operations, for sign-off.

<!-- 30 minutes. Goal: the tech lead accepts the design, or we leave with a list of changes. -->

---

## Architecture

- Web app → Items API → Items database; sign-in with the identity provider
- Stateless API on two instances; a managed database with point-in-time restore
- Diagrams: [architecture](../design/architecture.md), [workflows](../design/workflows.md)

---

## Decisions (ADRs)

| Decision | Choice | Why |
| --- | --- | --- |
| ADR-0001 | Cursor paging, 50 per page | No repeated or missing items while members add items |

---

## API contract

- `GET /items` lists my items, newest first; `POST /items` adds one
- One error shape, `{ "error": { "code", "message" } }`, for 400 and 401
- Every example in `spec/openapi.json` is checked against its schema

---

## Data and privacy

- Item names may contain personal details: kept until the member deletes them, never logged
- The owner comes from the token, never from the request
- [Data dictionary](../design/data-dictionary.md), [data flows](../design/dataflow.md)

---

## Threat model

- 7 threats across all STRIDE categories
- High: TM-1 stolen or forged tokens, TM-2 reading another member's items
- Both mitigations are planned and proven by security tests

---

## Test plan

- Contract tests send the contract's examples; UI, load and security tests
- Every acceptance criterion and every high-risk threat has a test

---

## Operations

- Feature flag `items-list`, ring rollout; rollback is turning the flag off
- SLO 99.9%, p95 under 300 ms; error budget and latency alerts with runbooks

---

## Open questions

- Do items need due dates?
- Is 50 items per page right on phones?
