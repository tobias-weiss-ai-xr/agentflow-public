# Capability: config

## MODIFIED Requirements

### Requirement: Environment overrides

`af` SHALL honor the environment variables `AF_REPO_DIR`, `AF_STATE_DIR`, `AF_MAX_PARALLEL`, `AF_BRANCH_PREFIX`, `AF_POLL`, `AF_GATE_ENV`, and `AF_PROMPT_FILE` with documented defaults, matching taskfleet semantics for drop-in compatibility. `AF_MAX_TOKENS` (campaign token spend ceiling, `0` = unlimited) SHALL be honored the same way, defaulting to `0`. `AF_PROMPT_FILE` SHALL override the prompt-template path, defaulting to `prompts/worker.md`; the template file SHALL be re-read for every rendered prompt. For every such variable af SHALL read the `AF_*` name first and, only when it is unset, fall back to the legacy `TF_*` name; when a legacy `TF_*` name is consulted af SHALL print a deprecation notice to stderr (`warning: TF_X is deprecated; use AF_X instead`) at most once per legacy name per process.

#### Scenario: overrides applied

WHEN `AF_MAX_PARALLEL=2` and `AF_POLL=5` are set in the environment
THEN the scheduler uses max 2 parallel tasks and a 5s poll interval.

#### Scenario: prompt template is selectable per campaign

WHEN `AF_PROMPT_FILE=/srv/wo/template.md` is set
THEN prompts render from that file instead of `prompts/worker.md`.

#### Scenario: legacy TF_* names still apply with a warning

GIVEN only `TF_MAX_PARALLEL=2` is set (no `AF_MAX_PARALLEL`)
WHEN settings are loaded
THEN the scheduler uses max 2 parallel tasks and stderr carries exactly one `TF_MAX_PARALLEL is deprecated; use AF_MAX_PARALLEL instead` notice.

#### Scenario: AF_* wins over the legacy twin

GIVEN both `AF_POLL=5` and `TF_POLL=9` are set
WHEN settings are loaded
THEN the poll interval is 5s and no deprecation notice is printed.

#### Scenario: defaults when unset

WHEN no `AF_*` or legacy `TF_*` variables are set
THEN defaults are used (max parallel = number of enabled workers, poll = 15s).

#### Scenario: AF_MAX_TOKENS parses with unlimited default

WHEN `AF_MAX_TOKENS=500000` is set in the environment
THEN the campaign token ceiling is 500000.
WHEN `AF_MAX_TOKENS` is unset
THEN the campaign token ceiling is `0` (unlimited).

### Requirement: Per-task turn cap and readonly task fields

`af` SHALL support two optional Task fields beyond the schema documented
above.

- `max_turns` — an integer >= 1 capping the number of LLM round-trips for a
  `cli: "builtin"` attempt of THIS task. It SHALL override the worker's
  `max_turns`, then `AF_AGENT_MAX_TURNS`, then the harness default of 32, only
  for this task. Validation SHALL reject `max_turns: 0` or negative with a
  hard error naming the task. Absent = no per-task override.
- `readonly` — a boolean (default `false`). When `true` the task is
  investigation-only: `af` SHALL complete the attempt when the agent stops
  normally, with no acceptance gate and no merge, and the builtin harness
  SHALL reject every `write` and `edit` tool call. Validation SHALL reject
  `readonly: true` combined with an `accept` gate — a gate on a read-only task
  is contradictory — with a hard error naming the task. Absent = normal task
  behavior unchanged.
- `max_tokens` — an optional non-negative integer (default `0` = no per-task
  cap). A per-task token ceiling: before each dispatch, when the task's own
  receipts already total at least its `max_tokens`, the task SHALL be set to
  `failed` with a reason naming the spend and the cap, and no further agent
  SHALL be dispatched for it. It composes with the campaign ceiling
  (`AF_MAX_TOKENS`) and with `max_turns`/`max_attempts`. Absent or `0` =
  no per-task cap.
- `cascade` — an optional ordered list of worker names (escalation chain,
  cheapest first). Attempt `N` of the task dispatches on `cascade[N-1]`;
  when the worker is busy, disabled, or the attempt is past the cascade, the
  router falls back to UCB1. Validation SHALL reject with a hard error
  naming the task: a cascade entry naming no worker in `workers.json`, a
  duplicated worker within one cascade, and an empty cascade array. Absent
  = UCB1 as before.

#### Scenario: per-task max_turns parses and validates

WHEN a task declares `max_turns: 12`
THEN it loads successfully.
WHEN a task declares `max_turns: 0`
THEN loading fails naming the task.

#### Scenario: readonly parses and validates

WHEN a task declares `readonly: true` and no `accept`
THEN it loads without the usual no-gate warning.
WHEN a task declares `readonly: true` and an `accept` command
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
