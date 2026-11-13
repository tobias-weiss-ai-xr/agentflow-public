# Capability: scheduling

## ADDED Requirements

### Requirement: Worker pool tracks the live enabled set

The scheduler SHALL derive its parallel capacity for each dispatch round from the *current* enabled-worker count when the operator left the ceiling to the default: `AF_MAX_PARALLEL` unset or `0` means "one slot per enabled worker, recomputed on every hot reload", so appending a worker to `workers.json` grows the pool and setting one `enabled: false` shrinks it, without a process restart. An explicit `AF_MAX_PARALLEL=N` SHALL be a fixed ceiling that is never recomputed, regardless of how the worker set changes. The pool capacity SHALL be at least `1` in every case. A reload that disables every worker SHALL be rejected by config validation, and the run loop SHALL keep the last good config (and its pool) with a warning — a running campaign can never be stranded with an empty pool by a malformed edit. In-flight attempts are unaffected: the change binds only the next dispatch round.

#### Scenario: default ceiling follows the live worker count

GIVEN a campaign launched with `AF_MAX_PARALLEL` unset and two enabled workers
WHEN a third enabled worker is added to `workers.json` while the campaign runs
THEN the next dispatch round allows up to three concurrent attempts.

#### Scenario: disabling a worker reduces the pool

GIVEN a campaign launched with `AF_MAX_PARALLEL` unset and three enabled workers
WHEN one worker becomes `enabled: false` while the campaign runs
THEN the next dispatch round allows up to two concurrent attempts and no new
attempt is routed to the disabled worker.

#### Scenario: explicit ceiling is never recomputed

GIVEN a campaign launched with `AF_MAX_PARALLEL=4`
WHEN the worker set grows to six while the campaign runs
THEN the pool capacity stays four.

#### Scenario: all-disabled reload keeps the last good pool

GIVEN a running campaign
WHEN the workers file is edited so that every worker is `enabled: false`
THEN config validation fails, the loop keeps the last good config and pool
with a single warning, and dispatch continues unchanged.
