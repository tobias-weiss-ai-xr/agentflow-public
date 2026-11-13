# Capability: scheduling

## ADDED Requirements

### Requirement: Adaptive backoff for infra-failing workers

When a worker's attempt fails as infrastructure (`failure_class: "infra"` — provider 4xx/5xx, rate limit, transport or harness timeout), `af run` SHALL cool that worker down before it can be picked again, escalating with each consecutive infra failure of the same worker: 30 s after the first, then 60 s, 120 s, 240 s, and a 300 s ceiling thereafter. Any verdict attempt for the worker (a merged, verified, or non-infra failed attempt) SHALL reset its escalation level to zero. A backed-off worker stays unpickable by the router exactly as today. When the pool has exactly one enabled worker, the cooldown SHALL not exceed 30 s regardless of escalation level, so a solo campaign is never locked out of dispatch for minutes. Cooldown state is per-worker and in-memory for the running process; the persistence of the underlying failures (receipts) already survives restart, so a fresh round after a crash re-derives the first cooldown level from the preceding infra failure.

#### Scenario: repeated infra failures cooldown longer

GIVEN a worker whose first infra failure set a 30 s cooldown
WHEN the same worker fails infra again before the cooldown elapsed (a third, fourth, fifth failure follow)
THEN its cooldown escalates 60 s → 120 s → 240 s → 300 s, and no new attempt is routed to it while its cooldown is active.

#### Scenario: one success resets the escalation

GIVEN a worker at its 300 s cap after four infra failures
WHEN it merges a task
THEN its escalation resets to level 0 (the next infra failure cools it down for 30 s again).

#### Scenario: a solo worker is never locked out for minutes

GIVEN a pool with exactly one enabled worker that keeps failing infra
WHEN consecutive infra failures occur
THEN the worker's cooldown stays at most 30 s and the campaign keeps retrying rather than idling out for 5 minutes.

#### Scenario: infra failures stay non-verdicts

GIVEN a worker whose only attempts all failed as infra
WHEN trust statistics are computed
THEN the receipts stay outside the verdict numerator and denominator exactly as before — only the cooldown pacing changed.
