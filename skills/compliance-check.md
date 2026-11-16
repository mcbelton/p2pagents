---
name: compliance-check
description: Check dependency licenses, committed secrets and required notices
---
# Compliance check

1. Licenses: list dependencies added or upgraded by the change with their licenses; flag copyleft (GPL, AGPL), unknown or missing licenses.
2. Secrets: confirm no keys, tokens, passwords or connection strings are committed (code, config, tests, history of this change).
3. Notices: third-party notices or attributions the licenses require are present.
4. Policy: anything the project's own policies (CONTRIBUTING, SECURITY, compliance docs) require.

## Output
A checklist with pass or fail per item and the evidence for each fail.
