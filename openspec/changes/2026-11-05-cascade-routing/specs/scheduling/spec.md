# Capability: scheduling

## ADDED Requirements

### Requirement: Cascade (escalation) routing

A task SHALL accept an optional `cascade` field: an ordered list of worker
names, CHEAPEST-FIRST. When set, attempt `N` of the task SHALL dispatch on
`cascade[N-1]` (the first attempt runs the cheapest worker, the second
escalates to the next, and so on), bypassing UCB1 selection for that attempt.
The acceptance gate SHALL remain the sole arbiter of work quality — a
cascade only changes WHO is dispatched, never whether work is accepted.
When the attempt number is past the end of the cascade, the named worker is
disabled, or the named worker is busy with another task, `af` SHALL fall
back to UCB1 selection exactly as for a task without a cascade. A task
without a `cascade` SHALL use UCB1 for every attempt (unchanged). Validation
SHALL reject, with a hard error naming the task: a cascade entry that names
no worker in `workers.json`, a duplicated worker name within one cascade
(escalation would repeat it), and an empty cascade array (treat as absent
instead). A cascade whose first entry names a disabled worker SHALL warn
non-fatally that every attempt will fall back to UCB1.

#### Scenario: first attempt runs the cheapest cascade worker

GIVEN a task with `cascade: ["cheap", "pro"]` and no failure history
WHEN attempt 1 dispatches
THEN it runs on `cheap` instead of a UCB1 pick.

#### Scenario: a failed attempt escalates to the next worker

GIVEN a task with `cascade: ["cheap", "pro"]` whose attempt 1 failed
WHEN attempt 2 dispatches
THEN it runs on `pro` (the attempt number indexes the cascade).

#### Scenario: past the cascade the router picks again

GIVEN a task with `cascade: ["cheap", "pro"]` whose first two attempts failed
WHEN attempt 3 dispatches
THEN UCB1 selection applies (no cascade entry remains).

#### Scenario: a busy or disabled cascade worker falls back to UCB1

GIVEN attempt `N` names cascade worker `W` and `W` is busy or disabled
WHEN the attempt dispatches
THEN it is not forced to wait for `W` — UCB1 picks among the free, enabled workers.

#### Scenario: a task without a cascade keeps UCB1

GIVEN a task with no `cascade` field
WHEN any attempt dispatches
THEN UCB1 selection applies for every attempt.

#### Scenario: an explicit worker pin overrides the cascade

GIVEN a task with a `cascade` and an explicit `--worker` filter on the run
WHEN any attempt dispatches
THEN the pinned worker is used (the operator's constraint wins over the task's escalation intent).
