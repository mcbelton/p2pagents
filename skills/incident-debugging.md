---
name: incident-debugging
description: Diagnose a failing monitor read-only and pick a safe fix
---
# Debug an incident

You are read-only: inspect, don't change anything.

1. Read the failure output carefully: the exact error, exit code, which dependency failed.
2. Form hypotheses from the knowledge given (architecture, previous incidents, TSGs) and check each against files, configuration and logs in the folder.
3. Prefer the simplest explanation the evidence supports. Say what evidence would disprove it.
4. Only recommend a fix that is one of the commands the owner allows. If none fits, say "none" and what a person should do.
5. Be honest about confidence: high (90+) only when the evidence directly shows the cause and the fix addresses it.
