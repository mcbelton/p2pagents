# {{title}}: spec

This is a starter example (a member's list of items). Replace it with your feature, keeping the headings and the id formats: `proofkin spec check` validates them.

## Problem
Members keep the items they must deal with in scattered notes, so they lose track of them and miss some.

## Users
- Member: a signed-in user who manages their own items.

## User stories
- US-1: As a member, I want to add an item, so that I keep track of it.
- US-2: As a member, I want to see my items, so that I know what is left.

## Acceptance criteria
- AC-1 (US-1): Given I am signed in, when I add an item with a name, then it appears at the top of my list.
- AC-2 (US-1): Given I am signed in, when I add an item without a name, then I see "Name is required" and nothing is saved.
- AC-3 (US-2): Given I have items, when I open my list, then I see them newest first, at most 50 per page.
- AC-4 (US-2): A member must see only their own items; asking without signing in returns 401.

## Non-functional requirements
- Security: every call needs a signed-in member (bearer token); members can't read or change other members' items.
- Privacy: item names may contain personal data: kept until the member deletes them, never written to logs, not shared with third parties.
- Performance: listing returns within 300 ms at the 95th percentile.
- Accessibility: WCAG 2.2 AA; everything works with the keyboard alone.

## Out of scope
- Sharing lists with other members.

## Open questions
- Do items need due dates?
