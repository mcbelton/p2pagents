---
name: review-product
description: Prepare the product (PM) review deck (Marp): user value, stories, prototype walkthrough, scope and success measures
---
# Product review deck

Read `spec/SPEC.md`, `prototype/`, and the design's operations plan for the release. Write `review/product.md`, a Marp deck. Checked by `review:decks`.

## Audience and tone
The product owner, product managers, design and UX research, customer-facing teams.
- **User-centred**: lead with the user's problem and the outcome for them, in the users' own words.
- **Concrete**: walk through the prototype's screens and states; show what a user sees, not how it is built.
- **Measurable**: success metrics with targets and how they are measured.
- **Clear on scope**: what is in, what is out and why, what is deferred.
- Little technical detail: mention the API or architecture only where it changes what users get.

## Format
A Marp deck (front matter `marp: true`; slides separated by `---`; each slide titled with `##`; speaker notes in `<!-- -->`). At most 20 slides.

## Slides (titles must name these topics)
1. Title: feature and review.
2. **Problem**: who has it, how often, what it costs them today.
3. **Users and stories**: every user story by id (US-1, US-2...) in plain words.
4. **Prototype walkthrough**: the main path and the error and empty states, step by step (open the prototype live).
5. **Scope**: in and out, with reasons.
6. **Success metrics**: targets and how they are measured.
7. **Release plan**: rollout steps and dates, customer feedback round.
8. **Decisions needed**: open questions for the product owner, including approving the customer feedback document.
