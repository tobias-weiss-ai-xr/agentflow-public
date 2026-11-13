# Capability: lifecycle

## MODIFIED Requirements

### Requirement: Prompt rendering

A task prompt SHALL be rendered from the configured prompt template — `prompts/worker.md` by default, or the file named by `AF_PROMPT_FILE` — with the task's `title`, `id`, `scope`, `acceptance_prose`, and `context` substituted, and written to a file passed to the agent CLI. The `{{CONTEXT}}` placeholder SHALL be replaced with the task's context (empty when the task declares none); a `{{#CONTEXT}}…{{/CONTEXT}}` conditional section in the template SHALL drop entirely when the task declares no context and render otherwise. For attempt ≥ 2, the rendered prompt SHALL also include a "Previous attempts" block listing this task's earlier failed attempts (attempt number + error line) so the agent avoids repeating them: when the template carries the `{{PREVIOUS_ERROR}}` token the block is substituted into it, otherwise it is appended to the prompt. Attempt 1 SHALL render without the block (the `{{PREVIOUS_ERROR}}` token, if any, renders empty). A configured template that omits `{{CONTEXT}}` for a task that declares context SHALL have the context appended as a "Task context" section (with a warning), so crafted context is never silently dropped. The template file SHALL be re-read for every rendered prompt, so an edited template takes effect on the next attempt without a restart.

#### Scenario: template substitution

WHEN a task with title and acceptance prose is dispatched
THEN the rendered prompt file contains the task title and acceptance prose.

#### Scenario: task context is injected

GIVEN a task declaring `context: "read docs/CONVENTIONS.md first"`
WHEN the prompt renders from a template containing `{{CONTEXT}}`
THEN the rendered prompt contains that exact context in place of the placeholder.

#### Scenario: conditional context section vanishes when unset

GIVEN a template containing `{{#CONTEXT}}Remember:{{CONTEXT}}{{/CONTEXT}}`
WHEN a task without context renders
THEN neither `{{#CONTEXT}}`, `{{CONTEXT}}`, nor the surrounding section text appears in the prompt.

#### Scenario: a task-context template that omits the placeholder is repaired

GIVEN a task with context and a custom template that contains no `{{CONTEXT}}`
WHEN the prompt renders
THEN the context appears as an appended `## Task context` section and a warning names the template.

#### Scenario: retry prompt names the earlier failure

GIVEN attempt 1 of task A failed with an agent exit code
WHEN attempt 2 renders its prompt
THEN the prompt contains a previous-attempts entry for attempt 1.

#### Scenario: first attempt has no history block

GIVEN a task dispatching its first attempt
WHEN the prompt renders
THEN no previous-attempts block appears (and the `{{PREVIOUS_ERROR}}` token, if any, renders empty).

#### Scenario: custom template is selected per campaign

GIVEN `AF_PROMPT_FILE` names a template that is otherwise complete but omits the exact gate command
WHEN a prompt renders
THEN the template is used and the auto-append hardening still adds the missing gate command section.

### Requirement: Dispatch rounds read the current task config

Each dispatch round of `af run` SHALL be planned against the current
tasks/workers files (mtime-gated hot reload), and a reload failure SHALL
degrade to the last good config with a single warning, never to a campaign
abort. When the operator left the parallel ceiling to the default
(`AF_MAX_PARALLEL` unset or `0`), the pool capacity for a round SHALL be
recomputed from the reloaded config's enabled-worker count, so adding or
disabling workers while the campaign runs grows or shrinks the pool on the
fly; an explicit `AF_MAX_PARALLEL` SHALL remain a fixed ceiling across
reloads. In-flight attempts finish on the worker they started and are
unaffected by either change.

#### Scenario: hot reload refreshes ExecCtx for dispatch

- Given a campaign mid-run
- When the workers file changes (e.g. a worker is disabled) between rounds
- Then subsequent dispatch picks reflect the change without a restart

#### Scenario: adding a worker mid-run grows the pool

- Given a campaign launched with the default ceiling (AF_MAX_PARALLEL unset) and two enabled workers
- When a third worker is appended to workers.json while `af run` is running
- Then the next dispatch round runs with a pool capacity of three, so three disjoint-scope tasks may run concurrently

#### Scenario: disabling a worker mid-run shrinks the pool

- Given a campaign launched with the default ceiling and three enabled workers
- When one worker is set `enabled: false` while `af run` is running
- Then the next dispatch round runs with a pool capacity of two and no new attempt is routed to the disabled worker

#### Scenario: an explicit ceiling stays fixed

- Given a campaign launched with `AF_MAX_PARALLEL=4` and two enabled workers
- When a third worker is appended to workers.json while `af run` is running
- Then the pool capacity remains four (the operator's explicit ceiling is never recomputed)

(Shared scenarios for failure handling live in the config delta.)
