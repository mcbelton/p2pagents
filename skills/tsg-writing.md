---
name: tsg-writing
description: Write a troubleshooting guide that an agent can run automatically
---
# Write a troubleshooting guide (TSG)

A TSG is Markdown with front matter that lets the monitor's agent fix a known failure without a model:

    ---
    tsg: <topic-name>
    monitor: <monitor-name>
    match: <short text that appears in the failure output>
    fix: <name of one of the folder owner's checks that fixes it>
    verify: <check that confirms the fix; default the monitor's check>
    status: draft
    ---
    # <symptom>

Then: symptoms, likely causes, how to diagnose (commands and what to look for), the fix and why it works, how to verify, when to escalate to a person. Use `status: draft` - a person reviews and sets it to `active`. The `fix` must be an existing check name; never invent commands.
