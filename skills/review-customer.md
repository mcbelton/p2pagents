---
name: review-customer
description: Write the customer feedback document: what we heard, what we're building, and questions for the customer, in plain, warm words
---
# Customer feedback document

Read `spec/SPEC.md` (problem, stories, out of scope) and the prototype. Write `review/customer-feedback.md`, a short document (not a deck) to share with customers before the feature is finished, to learn whether it fits. Checked by `review:decks`; the product owner approves sharing it.

## Audience and tone
Customers and their users. They don't know how we work inside.
- **Warm and plain**: thank them; short sentences; their words, not ours. Second person ("you").
- **About them**: their needs and benefits, not our architecture.
- **Honest and careful**: say what is not included. Promise no dates or features beyond what is approved; "about five weeks", "we plan to".
- **Nothing internal**: no internal ids (US-, AC-, ADR-, TM-, DF-), team or code names, security details (threat models, vulnerabilities), or anything confidential. No personal data: no names or email addresses of people.
- No code or diagram source; the prototype or screenshots can be shown in a call.

## Sections (`##` headings)
1. **What we heard**: their needs, as they told us.
2. **What we're building**: what they will be able to do.
3. **How it helps you**: the benefit for them.
4. **What's not included yet**: what's out of this version, and that we want to hear if it matters.
5. **Questions for you**: at least three open questions, each ending with "?", that would change what we build (priorities, order, missing needs, reasons to stop using it).
6. **Next steps**: how to answer (account team, a short call) and what happens with their feedback.
