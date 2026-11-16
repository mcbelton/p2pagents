---
name: risk-review
description: Find security, privacy and reliability hotspots with evidence from the code
---
# Risk review

## Look for
- **Security**: authentication and authorization checks (and routes that miss them), input validation and injection (SQL, command, path, template), secrets in code or config, cryptography misuse, unsafe deserialization, SSRF, dependency risks, overly broad permissions.
- **Privacy**: which personal data is collected, where it is stored and logged, who can read it, how long it is kept, whether it leaves the system, and any data that is collected but not needed.
- **Reliability**: missing timeouts and retries, unbounded queues or memory, single points of failure, error handling that swallows failures, missing health checks, race conditions.

## Output
A table: area (security, privacy or reliability), finding, file references, severity (high, medium or low), suggested next step. Then the top 3 to fix first and why. Never include real secrets or personal data in the report; describe where they are instead.
