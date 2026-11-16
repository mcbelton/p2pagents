# ADR-0001: Cursor-based paging for item lists

## Status

Accepted

## Context

The list shows a member's items newest first, at most 50 per page (AC-3). Members add items while they page through the list, and some members keep hundreds of items.

## Decision

`GET /items` returns at most `limit` items (default and maximum 50) and an opaque `next` cursor that encodes the creation time and id of the last item returned. The next page continues after that item.

## Alternatives considered

- **Offset paging** (`?page=3`): simple, but items added while a member pages shift every page, so items repeat or go missing.
- **Everything at once**: no paging, but responses grow without limit and the list gets slow.

## Consequences

- Pages stay correct while items are added, and every page is one indexed query.
- Clients can't jump to page N; the list only moves forward.
- Cursors are opaque, so the order can change later without breaking clients.
