---
name: prototype-ux
description: Build a clickable, self-contained UX prototype of the spec's stories to review before development
---
# Build the UX prototype

Read `spec/SPEC.md` (and `spec/openapi.json` if present) and write a clickable prototype in `prototype/`, starting at `prototype/index.html`. It is checked by `spec:ux` and `spec:trace`, and previewed in a sandbox.

## Rules
- **Self-contained**: plain HTML, CSS and a little JavaScript, inline or in files under `prototype/`. No CDN, web fonts, images from the web, `fetch` or `XMLHttpRequest`: the preview blocks the network.
- **Example data inline**: reuse the API examples so the screens show what the API returns.
- **Trace it**: mark each screen or section with the story it shows and the criteria it demonstrates:
  `<section data-story="US-1" data-ac="AC-1 AC-2">`. Every story in the spec must appear.
- **States**: show empty, loading, error (each 4xx from the contract that a user can hit) and success states; a small switcher or links between them is enough.
- **Navigation**: links or buttons between screens (hash links like `#screen-2` or show/hide); everything reachable from `index.html`.

## UX quality
- Light and dark themes (`prefers-color-scheme`), readable at 200% zoom, works at 360px wide.
- Accessible: semantic elements (`button`, `label`, `nav`, `main`), visible focus, text contrast at least 4.5:1, every input labeled, errors next to the field and announced (`role="alert"`).
- Plain words from the spec; one primary action per screen.
- No real personal data: use made-up names and values.
