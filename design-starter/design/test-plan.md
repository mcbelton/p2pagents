# {{title}}: test plan

A test for every acceptance criterion in `spec/SPEC.md` and every high-risk threat in `design/threat-model.md`. Each test's name goes into its criterion's claim in `evidence/claims.json`.

| Covers | Level | Test | Data |
| --- | --- | --- | --- |
| AC-1 | Contract | POST /items with the contract's example returns 201 and an Item matching the schema; GET /items then lists it first | The example of POST /items |
| AC-1 | E2E | In the UI, add "Renew the parking permit": it appears at the top | - |
| AC-2 | Contract | POST /items with an empty name returns 400 name_required and saves nothing | An empty name |
| AC-2 | UI | Pressing Add with no name shows "Name is required" by the field, announced to screen readers | - |
| AC-3 | Integration | With 60 items, the first page has the 50 newest, newest first; the next cursor returns the other 10 | 60 generated items |
| AC-4 | Contract | GET /items without a token returns 401; another member's token never returns these items | Two test members |
| TM-1 | Security | Expired, wrongly signed and wrong-audience tokens are refused with 401 | Forged tokens |
| TM-2 | Security | Member B can't list, read or change member A's items, even knowing their ids | Two test members |

## Non-functional testing

- **Performance:** a load test lists 50 items in under 300 ms at the 95th percentile with 100 members at once.
- **Security:** the tests above, plus dependency and secret scans in CI.
- **Accessibility:** automated checks of every screen and a keyboard-only pass.
