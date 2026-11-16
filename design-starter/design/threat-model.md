# {{title}}: threat model

STRIDE, per element and per data flow in `design/dataflow.md`.

## Assets

- Item names, which may contain personal details.
- Members' tokens and sessions.
- The availability of the list.

## Trust boundaries

- The member's device and the Items service (DF-1, DF-2).
- The member's device and the identity provider (DF-4).

## Threats

| ID | Element | STRIDE | Threat | Mitigation | Risk | Status |
| --- | --- | --- | --- | --- | --- | --- |
| TM-1 | DF-1, Items API | Spoofing | Someone calls the API with a stolen or forged token | Check the signature, issuer, audience and expiry on every call; short-lived tokens | High | Planned |
| TM-2 | DF-2, Items API | Information disclosure | A member reads another member's items | Every query filters by the token's member (AC-4); random ids | High | Planned |
| TM-3 | DF-1 | Tampering | A request changes another member's item or injects SQL | Parameterized queries; the owner comes from the token, never the body | Medium | Planned |
| TM-4 | Items API | Repudiation | A member disputes adding an item | Audit log of item creation: member id, time and item id, no names | Low | Planned |
| TM-5 | DF-1, Items API | Denial of service | Floods of requests or huge names exhaust the service | Rate limit per member; names up to 200 characters; pages up to 50 | Medium | Planned |
| TM-6 | DF-4, Items API | Elevation of privilege | A member reaches operations meant for administrators | The API has no admin operations; roles are checked on the server | Low | Accepted |
| TM-7 | Logs | Information disclosure | Item names leak into logs or error messages | Never log request bodies or item names; errors don't repeat input | Medium | Planned |

## Residual risks

- TM-6 is accepted: there are no admin operations to protect yet. Revisit when there are.
