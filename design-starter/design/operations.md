# {{title}}: operations

How the feature is released, watched and fixed.

## Rollout

- Behind the feature flag `items-list`, off by default.
- Rings: the team first, then 10% of members, then everyone over a week, checking the error budget and alerts at each step.
- The database change only adds a table, so the previous version keeps working.

## Rollback

- Turn the flag off: no deployment needed, and members see the previous experience.
- For a bad release, redeploy the previous version; the additive schema needs no down-migration.

## Monitoring

- SLO: 99.9% of item requests succeed (no 5xx) over 28 days; 95th percentile latency under 300 ms.
- Metrics: requests, errors and latency per route; items added per day; rate-limited requests.
- Dashboard: "Items health": the SLO and error budget, errors, latency and traffic.
- Logs carry the request id and member id, never item names.

## Alerts

| Alert | Condition | Severity | Action |
| --- | --- | --- | --- |
| Error budget burn | 5xx above 1% for 10 minutes | Sev 2 | Runbook: API errors |
| Latency | p95 > 300 ms for 15 minutes | Sev 3 | Runbook: slow list |

## Runbook

### API errors

1. Open the "Items health" dashboard: did errors start with a deployment or a flag change?
2. If so, turn the flag off or redeploy the previous version, and check that errors fall.
3. Otherwise check the database health and the identity provider's status page.

### Slow list

1. Check that the list query uses the index on owner and creation time.
2. Check the database load; scale it up if it is saturated.
