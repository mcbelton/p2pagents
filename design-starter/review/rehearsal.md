# {{title}}: review rehearsal

Agents played each reviewer, read the review pack and raised what they would raise in the real reviews. This is what to fix or prepare before them.

## Readiness

Ready with changes: two major concerns need an answer before the security and leadership reviews; nothing blocks.

## By audience

### Technical

Likely verdict: approve with changes. The tech lead will ask why cursor paging and not offsets, and how the list behaves with thousands of items.

### Security

Likely verdict: approve with changes. Token checks are in place; the security champion will want the rate limit per member tested, not only planned.

### Privacy

Likely verdict: approve. Item names stay with their owner and are never logged; retention follows the member's account.

### Product

Likely verdict: approve. The product owner will ask how we measure the 60% goal in the first week.

### Leadership

Likely verdict: approve with changes. Leadership will ask what happens if the early preview goes badly, and whether two engineers is enough.

### Customer

Likely verdict: approve. Customers will ask whether they can share a list, which is out of this version.

## Actions

| # | Concern | Audience | Severity | Action | Owner |
| --- | --- | --- | --- | --- | --- |
| 1 | The rate limit per member is planned but not tested | Security | Major | Add a load test for the limit to the test plan before the security review | Tech lead |
| 2 | No fallback if the early preview goes badly | Leadership | Major | Add the switch-off and rollback steps to the leadership deck's Risks slide | Product owner |
| 3 | How the 60% first-week goal is measured | Product | Minor | Name the metric and its dashboard in the product deck | Product owner |
| 4 | Sharing lists is not included | Customer | Minor | Keep it under "What's not included" and ask how much it matters | Product owner |

## Prepared answers

**Why cursor paging and not page numbers?**
Items are added while members scroll; a cursor never skips or repeats an item (see the decision record).

**What if a member has thousands of items?**
Pages hold up to 50 items and the query uses the member's index, so the time per page stays the same.

**Can we switch it off quickly?**
Yes. The feature flag turns it off within minutes, and the rollout goes step by step.

## Open questions

- Should a member be able to export the list in the first version?
