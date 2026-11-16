---
name: threat-modeling
description: Threat-model the design with STRIDE over its data flows and trust boundaries, with mitigations and risk
---
# Threat model (STRIDE)

Read `design/dataflow.md`, `design/architecture.md`, `design/data-dictionary.md` and the spec's security and privacy requirements. Write `design/threat-model.md`. Checked by `design:threats`; the security champion signs off on it.

## Sections
1. **Assets**: what an attacker wants: personal data, credentials and tokens, money, availability, integrity of records.
2. **Trust boundaries**: each subgraph boundary of the data flow diagram and the flows (`DF-n`) that cross it.
3. **Threats**: one table:

| ID | Element | STRIDE | Threat | Mitigation | Risk | Status |
| --- | --- | --- | --- | --- | --- | --- |

   - **Element**: the flows (`DF-1`) and components the threat targets. Every flow that crosses a trust boundary has at least one threat.
   - **STRIDE**: Spoofing, Tampering, Repudiation, Information disclosure, Denial of service, Elevation of privilege. Consider all six for every boundary-crossing flow and store; the model needs at least one of each, or a row saying why a category doesn't apply.
   - **Threat**: a concrete attacker action ("a member reads another member's items by changing the id"), not a category name.
   - **Mitigation**: the specific control (validate token audience and expiry, filter every query by owner, parameterized queries, rate limits, audit log without personal data).
   - **Risk**: High, Medium or Low (likelihood × impact). **Status**: Open, Planned, Mitigated or Accepted. A High risk can't stay Open; Accepted needs a reason under Residual risks.
4. **Residual risks**: what remains, who accepted it, when to revisit.

## Checklist
Authentication and session handling; authorization on every object (IDOR); input validation and injection; secrets and keys; logging of personal data; rate limits and resource exhaustion; dependencies and supply chain; admin and support access; data deletion and export (privacy rights).

Every High risk needs a test proving its mitigation in `design/test-plan.md`.
