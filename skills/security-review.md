---
name: security-review
description: Security review of a change against common vulnerability classes
---
# Security review

Review the change (the diff) and the code paths it touches for:
- injection (SQL, command, path traversal, template, LDAP, XPath);
- broken authentication or authorization (missing checks, IDOR, privilege escalation);
- sensitive data exposure (secrets in code, logs or errors; weak or missing encryption);
- unsafe deserialization and SSRF;
- vulnerable or unnecessary dependencies added;
- missing input validation and output encoding;
- denial of service (unbounded input, regex, recursion, allocation).

## Output
A table: finding, severity (critical, high, medium or low), file and line, exploit scenario in one line, fix. If there are none, say exactly "No high-severity findings" and list what you checked.
