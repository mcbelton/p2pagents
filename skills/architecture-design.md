---
name: architecture-design
description: Design the architecture from the spec, with context, component, UML and workflow diagrams (Mermaid)
---
# Design the architecture

Read `spec/SPEC.md`, `spec/openapi.json`, `prototype/` and the knowledge given (existing architecture, risks). Write `design/architecture.md` and `design/workflows.md`. Checked by `design:arch`.

## design/architecture.md (use these `##` headings)
1. **Context**: who and what the system talks to, in two or three sentences, plus a context diagram: a Mermaid `flowchart LR` (or `C4Context`) with people, this system and external systems, edges labeled with what flows and how (HTTPS, events, SQL).
2. **Components**: a table (Component | Responsibility) and a component diagram (Mermaid `flowchart TB`, components as nodes, a `subgraph` per deployable unit). Reuse the existing system's components; name new ones by what they do.
3. **Domain model**: a UML class diagram (`classDiagram`) or ER diagram (`erDiagram`) of the entities, their key attributes and relationships with multiplicities.
4. **Deployment**: where each component runs, how many instances, regions, stateful stores, backup and restore.
5. **Quality attributes**: a table (Attribute | Target | How) for security, privacy, performance, availability and accessibility, with numbers from the spec's non-functional requirements.
6. **Decisions**: one line per architecture decision, `- ADR-0001: short title`. Write each as an ADR (see the adr skill). Cite ADR ids only here.

## design/workflows.md
- One `##` section per workflow, titled with the user story it implements (`## Add an item (US-1)`); every story in the spec appears.
- A UML sequence diagram (`sequenceDiagram`) per workflow: actor, web app, API, stores and external services, the HTTP calls of the contract (method and path) and the error paths with `alt`/`else`.
- At least one state diagram (`stateDiagram-v2`) of an entity or a screen, or an activity diagram (`flowchart`) of a process.

## Rules
- Mermaid only, in ```` ```mermaid ```` blocks: they render on GitHub and in VS Code. Keep labels short; avoid parentheses and quotes inside node labels.
- Design what the spec asks, no more: anything else goes in an ADR's alternatives or the spec's open questions.
- Name the security boundary of every call: who is authenticated, where authorization is checked.
