---
name: api-contract
description: Design the REST API contract (OpenAPI 3 JSON) from the spec, with examples, before implementing it
---
# Design the API contract

Read `spec/SPEC.md` and write `spec/openapi.json` (OpenAPI 3.0 or 3.1, as JSON). It is checked by `spec:api` and `spec:trace`.

## Resources and operations
- Nouns for paths (`/orders`, `/orders/{orderId}`), HTTP methods for actions: GET reads (safe), POST creates (201 + `Location`), PUT replaces, PATCH updates partly, DELETE removes (204). Make PUT and DELETE idempotent.
- Every operation has a unique `operationId`, a `summary`, and `x-acceptance`: the acceptance criteria it serves, e.g. `"x-acceptance": ["AC-1", "AC-3"]`.
- Lists are paged (`limit`, `cursor`) and return `{ "items": [...], "next": "..." }`.

## Schemas and examples
- Put shared shapes in `components.schemas` and use `$ref`. List `required` properties and give each a `type`; use `enum` for fixed values and `format` (date-time, uuid, email).
- Every JSON request and response has an `example` (or `examples`) that matches its schema exactly: the check validates them, and the prototype and tests reuse them.

## Errors
- Every operation documents a success (2xx) and the client errors (4xx) it can return: 400 invalid input, 401 not signed in, 403 not allowed, 404 not found, 409 conflict, 422 rule broken, 429 too many requests.
- One error shape for all: `{ "error": { "code": "...", "message": "...", "details": [...] } }` (or RFC 9457 problem+json).

## Security and privacy
- Declare `components.securitySchemes` and `security` (global or per operation); say which role may call what.
- Return only the fields the caller needs; never personal data in paths or query strings; no secrets in examples.

## Versioning
- `info.version` follows semver; additive changes only within a major version.
