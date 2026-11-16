---
name: review-technical
description: Prepare the technical review deck (Marp) for engineers and architects: precise, evidence-backed, decision-focused
---
# Technical review deck

Read the spec package and the whole design package. Write `review/technical.md`, a Marp deck. Checked by `review:decks`.

## Audience and tone
Engineers, architects, the security champion. They will read every word and challenge it.
- **Precise and evidence-backed**: name components, operations (`POST /items`), ids (ADR-0001, TM-2, AC-3), numbers (p95 300 ms, 99.9%). No adjectives where a number fits.
- **Decision-focused**: state each decision, its alternatives and the trade-off; flag what you are unsure of.
- **Honest about risk**: show the high risks and what is still open, not only what is done.

## Format
```markdown
---
marp: true
paginate: true
---

# Feature name
## Technical review

<!-- Speaker notes: timing, goal of the meeting, what you need from the room. -->

---

## Architecture
...
```
- Slides separated by `---`; each starts with a `##` title (the first with `#`). At most 30 slides; one idea per slide.
- Speaker notes in `<!-- ... -->` comments under a slide.
- Link to the design documents for detail (`[threat model](../design/threat-model.md)`) rather than pasting them; a small Mermaid diagram is fine.

## Slides (titles must name these topics)
1. Title: feature, review, what decision you want.
2. **Architecture**: context and components in a few bullets; link the diagrams.
3. **Decisions (ADRs)**: a table of each ADR id, the choice and why.
4. **API**: the operations, error model, versioning, compatibility.
5. **Data and privacy**: personal data, classification, retention, boundary flows.
6. **Threat model**: STRIDE coverage, each High risk by id with its mitigation.
7. **Test plan**: levels, how criteria and threats are proven.
8. **Operations**: rollout, rollback, SLOs, alerts.
9. **Open questions**: the decisions needed today, as questions.
