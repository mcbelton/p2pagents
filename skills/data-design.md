---
name: data-design
description: Write the data dictionary (classification, purpose, retention) and the data flow diagram with trust boundaries
---
# Data dictionary and data flows

Read `spec/SPEC.md`, `spec/openapi.json` and `design/architecture.md`. Write `design/data-dictionary.md` and `design/dataflow.md`. Checked by `design:data`; privacy reviewers sign off on them.

## design/data-dictionary.md
One table with these columns:

| Entity | Field | Type | Required | Classification | Purpose | Retention | Description |
| --- | --- | --- | --- | --- | --- | --- | --- |

- A row for **every property of every schema** in `spec/openapi.json` (Entity = schema name, Field = property name), and for every field stored that the API doesn't show (owner ids, audit fields, foreign keys).
- **Classification**: Public, Internal, Confidential, Personal (identifies or relates to a person, including free text that may contain personal details) or Sensitive (special categories: health, finances, precise location, credentials, government ids).
- **Personal and Sensitive** rows need a **Purpose** (why we need it, tied to a story) and a **Retention** (how long, and what deletes it). If there is no purpose, remove the field.
- Add a domain model (`erDiagram`) if the architecture doesn't have one.

## design/dataflow.md
- A data flow diagram: Mermaid `flowchart LR` with processes, stores (`[(name)]`), external entities, and every **trust boundary as a `subgraph`** (device, service, partner, third party). Label edges with flow ids (`DF-1`).
- A table of flows:

| ID | From | To | Data | Boundary | Protection |
| --- | --- | --- | --- | --- | --- |

  - **Data** names the dictionary entities that move (`NewItem (name), bearer token`); every entity with personal data appears in some flow.
  - **Boundary**: Yes when the flow crosses a trust boundary. Those need **Protection**: transport encryption, authentication, authorization, minimization.
- Include flows to logs, analytics, backups and third parties: that is where personal data leaks.
