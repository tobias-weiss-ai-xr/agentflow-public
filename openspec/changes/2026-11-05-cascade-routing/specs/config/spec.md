# Capability: config

## MODIFIED Requirements

### Requirement: Per-task turn cap and readonly task fields

`af` SHALL support optional Task fields beyond the schema documented above:
`max_turns` (per-task turn cap for the builtin harness, overrides worker /
`AF_AGENT_MAX_TURNS` / default 32; `0` rejected), `readonly` (investigation-only:
builtin harness rejects every write/edit, completes on normal stop, no gate,
no merge; incompatible with an `accept` gate), `max_tokens` (per-task token
ceiling: before each dispatch, when the task's own receipts already total at
least `max_tokens`, the task is set `failed` and no further agent is
dispatched; 0/absent = no cap), and `cascade` (ordered escalation list of
worker names, cheapest first — attempt `N` dispatches on `cascade[N-1]`,
falling back to UCB1 when the worker is busy, disabled, or the attempt is
past the cascade).

#### Scenario: per-task max_turns parses and validates

WHEN a task declares `max_turns: 12`
THEN it loads successfully.
WHEN a task declares `max_turns: 0`
THEN loading fails naming the task.

#### Scenario: per-task max_tokens parses

WHEN a task declares `max_tokens: 5000`
THEN it loads with that cap.
WHEN a task omits `max_tokens`
THEN it loads with no per-task cap (`None`).

#### Scenario: per-task max_tokens zero is a no-op

WHEN a task declares `max_tokens: 0`
THEN it loads with no per-task cap (0 is a no-op).

#### Scenario: cascade must name real workers

WHEN a task declares a `cascade` entry that names no worker in `workers.json`
THEN loading fails naming the task and the worker.

#### Scenario: cascade entries must be unique

WHEN a task's `cascade` lists the same worker more than once
THEN loading fails naming the task and the repeated worker.

#### Scenario: empty cascade is rejected

WHEN a task declares `cascade: []`
THEN loading fails naming the task (an empty array is a config bug — omit it instead).
