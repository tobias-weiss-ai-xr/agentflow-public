# Proposal: dynamic-workers-and-task-context

## Why

Two operator asks from campaign usage (World-Office parity loop on legion,
dogfood campaigns, openSME series):

1. **Change the worker pool on the fly.** `af run` already hot-reloads
   `tasks.json`/`workers.json` by mtime (feedback §6.2), and a disabled /
   newly-added worker is picked up by UCB1 routing within one poll. But the
   parallelism ceiling is **frozen at startup**: `max_parallel` is derived
   once from the *startup* enabled-worker count (`src/run.rs:632`), so adding
   workers mid-run never unlocks more concurrency — the new worker just
   competes for the same fixed slots. With the default `AF_MAX_PARALLEL=0`
   (one slot per enabled worker), "scale up while it runs" is impossible
   without pre-declaring a ceiling you may never need.

2. **Craft the context for the workers.** Today the only per-task lever is
   `acceptance_prose` inside a single global template. There is **no
   per-task context field** (`context`/`instructions`/`mission`) to give one
   task extra repo facts, conventions, or a breakdown of what "done" means.
   And the shipped template has **drifted**: `prompts/worker.md` references
   `{{PREVIOUS_ERROR}}`, `{{EPISODES}}`, `{{MISSION}}`, but `render_prompt`
   (`src/execute.rs:1074`) substitutes only `{{TASK_ID}} {{TASK_TITLE}}
   {{SCOPE}} {{ACCEPTANCE}} {{ACCEPT_CMD}} {{MODEL}} {{PROVIDER}}` and
   *appends* the retry-failure block — the three template tokens pass through
   verbatim to the agent. The template path is also hardcoded
   (`Settings.prompt_file` → `prompts/worker.md`, no env override), so a
   campaign cannot point at its own template without overwriting the repo
   default.

Task-sizing evidence used to keep gates/turn-caps sane in the spec scenarios:
151 receipts across `state-*` show merged tasks at median 15 turns / mean 24 /
p90 50, and the "default 12-16 turns" guidance in README is too tight
(`examples-simple@16`, `doc-troubleshoot@14` died on turn caps).

## What Changes

**A. The worker pool follows the live enabled set.**

- `max_parallel` recomputes on every successful hot reload: when
  `AF_MAX_PARALLEL` is `0` (default), the pool size equals the *current*
  enabled-worker count — grow by appending a worker to `workers.json`,
  shrink by setting `enabled: false`. An explicit `AF_MAX_PARALLEL=N` stays
  a fixed operator ceiling (never recomputed). In-flight attempts finish on
  the worker they started; the change only affects future dispatch rounds.
- Behavior composes with existing safeguards: a reload that disables *all*
  workers fails validation (`config.rs:686`) and the hot-reload path keeps
  the last good config with a warning — a campaign can never be stranded
  with a dead pool by a typo.

**B. Per-task crafted context.**

- New optional Task field `context`: a string (or array of strings joined by
  newlines) injected into the worker prompt. Rendered through the
  `{{CONTEXT}}` template placeholder; when the template omits
  `{{CONTEXT}}`, the context is appended (mirroring the existing
  SCOPE/ACCEPTANCE/ACCEPT_CMD hardening). `{{#CONTEXT}}…{{/CONTEXT}}`
  conditional markers are dropped like `{{#SCOPE}}`. Absent/empty context
  renders to nothing.
- The retry-failure block substitutes into `{{PREVIOUS_ERROR}}` when the
  template carries the token (and is still appended at the end for
  templates that do not). The dead `{{EPISODES}}` / `{{MISSION}}` tokens are
  removed from the shipped `prompts/worker.md` and the template is reworded
  to use `{{CONTEXT}}` / `{{PREVIOUS_ERROR}}`.
- New `AF_PROMPT_FILE` env var (with legacy `TF_PROMPT_FILE` fallback per the
  `env_fallback` rule): overrides the prompt-template path, so a campaign
  (e.g. the World-Office loop on legion) can ship its own template next to
  its `tasks.json`/`workers.json` without touching the repo default. The
  template file is re-read per render, so editing it mid-run already takes
  effect on the next attempt.

## Non-Goals

- **No per-worker system prompt / per-worker context.** A worker-scoped
  template is speculative; the campaign-scoped `AF_PROMPT_FILE` plus the
  per-task `context` covers the observed need.
- **No cross-task episode recall.** `{{EPISODES}}` stays ditched — ADR-13
  explicitly defers cross-task recall ("per-task failure context is the
  honest, useful subset"). Per-task `context` is the per-task "mission".
- **No mid-attempt reconfiguration.** Changing workers/template/context only
  affects the *next* dispatch round; an in-flight attempt keeps its worker
  and its rendered prompt.
- **No change to CLI symbols or routing math.** UCB1 (ADR-12), cascades and
  `--worker` pins are untouched; this only changes where the parallel ceiling
  comes from and what text enters the prompt.
- **No taskfile/tooling changes.** Operators keep writing data-only
  `taskfile/examples/<campaign>.spec.py`; this change extends what a task
  may declare (one new field) and how the template is located (one new env).
