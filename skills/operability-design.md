---
name: operability-design
description: Plan how the feature is released, observed and fixed: rollout, rollback, SLOs, metrics, dashboards, alerts, runbooks
---
# Operations plan

Read `design/architecture.md`, the spec's non-functional requirements and the knowledge given (existing monitors, incidents, TSGs). Write `design/operations.md`. Checked by `design:ops`.

## Sections (`##` headings)
1. **Rollout**: feature flag, rings or percentages, what is checked before each step, data migrations (additive first) and their order with the deployment.
2. **Rollback**: how to undo within minutes (flag off, redeploy the previous version), and what can't be undone (data written by the new version) and how that is handled.
3. **Monitoring**:
   - **SLO** lines with targets: `SLO: 99.9% of item requests succeed (no 5xx) over 28 days`, plus a latency target.
   - **Metrics**: request rate, errors, latency per operation; business metrics from the product's success measures; saturation of stores.
   - **Dashboard**: its name and what it shows (SLO and error budget first).
   - **Logs**: what is logged, with correlation ids; never personal data or secrets.
4. **Alerts**: a table:

| Alert | Condition | Severity | Action |
| --- | --- | --- | --- |

   Alert on symptoms users feel (error budget burn, latency), not on every cause. Every alert's Action names a runbook.
5. **Runbook**: one `###` section per alert: how to confirm, the likely causes in order, the fix for each, and how to check it worked. Write them so they can become TSGs (`proofkin kb add`) with monitors (`proofkin monitor add`).
