# Capability: config

## MODIFIED Requirements

### Requirement: Task schema loading

`af` SHALL load a `tasks.json` file of the form `{ "_meta": {...}, "tasks": [...] }` into typed `Task` records. Each task SHALL support the fields `id`, `title`, `deps`, `scope`, `touch`, `accept`, `acceptance_prose`, `manual`, `repo`, `priority` (a number, or the levels `LOW`/`MEDIUM`/`HIGH`/`CRITICAL`), plus `max_turns`, `max_tokens`, `readonly`, `cascade` (documented in the per-task field requirements), and `context` — an optional free-form context string, or an array of strings joined with newlines, injected into the agent prompt via the `{{CONTEXT}}` placeholder. A `context` that is absent, `null`, an empty string, or an empty array SHALL normalize to no context. `touch` is an optional array of file paths the operator believes the agent must edit; absent or empty SHALL be the normal case with no behaviour change. Validation SHALL reject a `touch` entry that is covered by no `scope` entry — a hard error naming the task, the entry, and the scope — because such a task is unpassable by construction: it must edit a file its own scope forbids. Coverage SHALL be decided by the same matcher enforcement uses (`scheduler::scope_overlap`), so validation can never accept a config that enforcement will later reject; an empty `scope` means "any file" and covers every `touch` entry; a `touch` entry naming a file that does not exist yet SHALL be accepted (the task may create it). Validation SHALL reject: duplicate task ids and dependency cycles. It SHALL warn (without failing) on: `deps` referencing ids not present in the file (taskfleet composes configs across files), and tasks that are neither `manual` nor have an `accept` gate (the corpus runs these gate-less).

#### Scenario: valid config loads

WHEN a `tasks.json` with two tasks and one dependency is loaded
THEN both tasks are parsed and the dependency graph resolves without error.

#### Scenario: duplicate id rejected

WHEN a `tasks.json` contains two tasks with the same `id`
THEN loading fails with an error naming the duplicate id.

#### Scenario: dependency cycle rejected

WHEN a task's `deps` (transitively) reference the task itself
THEN loading fails with an error naming the cycle.

#### Scenario: dangling dependency warns

WHEN a task's `deps` references an id that does not exist in the file
THEN loading succeeds and emits a warning naming the missing dep.

#### Scenario: task without gate and not manual warns

WHEN a task has neither `accept` nor `manual: true`
THEN loading succeeds, warns, and the gate is skipped at run time.

#### Scenario: string priority levels accepted

WHEN a task declares `priority: "HIGH"` and another `priority: 3`
THEN both parse and rank deterministically (CRITICAL > HIGH > number-ranked).

#### Scenario: touch entry not covered by scope is rejected

GIVEN a task declaring `touch: ["src/router.rs"]` whose `scope` omits any entry covering it
WHEN `tasks.json` is loaded
THEN loading fails with an error naming the task, the touch entry, and the scope entries (so the fix — widen `scope` or drop the entry — is obvious), while a fully covered `touch`, an empty `touch`, and an empty `scope` all load unchanged.

#### Scenario: task context parses as a string

WHEN a task declares `"context": "repo conventions live in docs/CONVENTIONS.md — read them first"`
THEN it loads and the string renders into the prompt's `{{CONTEXT}}` placeholder.

#### Scenario: task context parses as an array

WHEN a task declares `"context": ["invariant: zero runtime deps", "gate is replay-safe: keep it that way"]`
THEN it loads and the entries render joined by newlines into the prompt's `{{CONTEXT}}` placeholder.

#### Scenario: empty or absent context is a no-op

WHEN a task omits `context`, or declares `"context": ""` or `"context": []`
THEN it loads with no context (the `{{CONTEXT}}` placeholder and its `{{#CONTEXT}}…{{/CONTEXT}}` section render empty).

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
THEN defaults are used (max parallel = number of enabled workers, poll = 15s, prompt template = `prompts/worker.md`).

### Requirement: Task config hot-reload between dispatch rounds

`af run` SHALL re-read tasks.json/workers.json when their mtimes change,
so a corrected scope/accept definition reaches the next retry without a
process restart. On an unparseable or invalid reload, the loop SHALL keep
the last good config, warn once per broken generation, and continue. A
reload that yields a config with zero enabled workers SHALL be treated as
invalid (schema validation rejects it) and SHALL keep the last good config.

#### Scenario: scope fix reaches the next retry without restart

- Given a running campaign whose task failed an attempt on a too-narrow scope
- When the operator edits the tasks file to widen the scope while `af run` runs
- Then the next dispatch round uses the widened scope (no restart needed)

#### Scenario: broken config file keeps the last good config

- Given a running campaign
- When the tasks file is edited into invalid JSON
- Then the loop keeps dispatching with the last good config and warns once
- And after the operator fixes the file, the next round loads the fix

#### Scenario: disabling every worker keeps the last good pool

- Given a running campaign with `enabled` workers
- When the workers file is edited so every worker is `enabled: false`
- Then the reload fails schema validation, the loop keeps the last good
  config (and its pool) and warns once
- And re-enabling at least one worker loads on the next round
