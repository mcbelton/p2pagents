---
name: review-leadership
description: Prepare the leadership team (LT) review deck (Marp): bottom line up front, impact, timeline, risks and the ask
---
# Leadership review deck

Read the spec's problem and success measures, the product deck, the design's risks and the operations plan. Write `review/leadership.md`, a Marp deck. Checked by `review:decks`.

## Audience and tone
Executives and leadership team members with ten minutes and many other decisions.
- **Bottom line up front**: the first slide after the title is the summary: what, why now, when, cost, and the ask.
- **Outcomes, not mechanics**: customer and business impact, measured. No architecture, APIs, code or diagrams source.
- **No internal ids or jargon**: never `US-1`, `AC-3`, `ADR-0001`, `TM-2`, `DF-1`, acronyms or team shorthand. Say what they mean.
- **Short**: under 90 words per slide, at most 12 slides. Put detail in speaker notes (`<!-- -->`).
- **Confident and candid**: state risks plainly with how they are handled; don't oversell.
- **One clear ask**: the decision, the budget or people, and by when.

## Format
A Marp deck (front matter `marp: true`; slides separated by `---`; each titled with `##`).

## Slides (titles must name these topics)
1. Title: feature and review.
2. **Summary**: three bullets: what we will do, when it is ready and with whom, the ask.
3. **Why it matters** (impact): the customer problem and the measurable outcome.
4. **Timeline**: milestones in weeks or dates.
5. **Risks**: the two or three that matter, each with its mitigation, in plain words (privacy, quality, schedule).
6. **Ask**: the decisions and resources needed.
