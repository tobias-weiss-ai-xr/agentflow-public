# Design: dynamic-workers-and-task-context

## Context

- `max_parallel` is computed once, before the dispatch loop, from the
  **startup** enabled-worker count (`src/run.rs:631-635`). Hot reload
  (`hot_reload`, `src/run.rs:533`) already swaps `cfg`/`ctx.cfg` for new
  rounds, but the loop's `running < max_parallel` gate uses the frozen
  value.
- `render_prompt` (`src/execute.rs:1074`) substitutes exactly
  `{{TASK_ID}} {{TASK_TITLE}} {{SCOPE}} {{ACCEPTANCE}} {{ACCEPT_CMD}}
  {{MODEL}} {{PROVIDER}}`, strips `{{#SCOPE}}/{{/SCOPE}}`, and *appends* the
  failure-context block. `prompts/worker.md` carries three un-substituted
  tokens (`{{PREVIOUS_ERROR}}`, `{{EPISODES}}`, `{{MISSION}}`).
- `Settings.prompt_file` is hardcoded (`src/config.rs:847`) with no env
  override; `Settings::from_env` is the single construction site
  (`src/main.rs:196`).
- Validation rejects a config with zero enabled workers (`src/config.rs:686`),
  which makes "all workers disabled" a *reload failure* → keep last good
  config + warn. No new empty-pool handling needed.

## Goals / Non-Goals

- Goals: a running campaign's parallelism tracks the live enabled worker set
  (when the operator left the ceiling to the default); tasks can carry
  crafted per-task context; a campaign can select its own prompt template;
  dead template tokens stop leaking into prompts.
- Non-Goals: per-worker system prompts; cross-task episode recall
  (`{{EPISODES}}` removed permanently); mid-attempt reconfiguration;
  reworking UCB1/cascade/`--worker` dispatch math.

## D1 — Live pool capacity (`src/run.rs`)

Extract a pure helper and make `max_parallel` mutable:

```rust
/// Parallel capacity for the *next* dispatch round. When the operator left
/// the ceiling to the default (AF_MAX_PARALLEL == 0), it tracks the CURRENT
/// enabled-worker count so a hot-reloaded worker pool grows/shrinks the
/// pool on the fly; an explicit ceiling stays fixed.
fn pool_capacity(st: &Settings, cfg: &Config) -> usize {
    if st.max_parallel == 0 {
        cfg.workers.iter().filter(|w| w.enabled).count().max(1)
    } else {
        st.max_parallel
    }
}
```

- Startup keeps printing the initial value; the loop variable becomes
  `let mut max_parallel = pool_capacity(st, &cfg);`.
- In the hot-reload branch (after `cfg = fresh; ctx.cfg = cfg.clone();`),
  recompute: `max_parallel = pool_capacity(st, &cfg);` and, when it changed,
  log `  max_parallel now {n} ({k} enabled worker(s))` so an operator sees
  the pool live-grow.
- No other change to the dispatch gate (`running < max_parallel`), UCB1
  selection, or `--worker` handling.

**Tests (unit, `src/run.rs` tests):** `pool_capacity` with AF_MAX_PARALLEL=0
counts enabled workers (2 → 3 when a third worker is added to the reloaded
config); with a fixed ceiling it returns the ceiling regardless of the pool;
with 0 enabled it clamps to 1 (unreachable via load validation, guarded
anyway). Extend the existing `hot_reload_picks_up_a_changed_tasks_file` test
family with a worker-set change (add one, disable one) asserting the reloaded
config drives capacity.

## D2 — Per-task `context` field (`src/config.rs`, `src/execute.rs`)

**Config.** Add to `Task`:

```rust
/// Free-form per-task context injected into the worker's prompt via the
/// `{{CONTEXT}}` placeholder (`#[serde(default)]`; string or array).
pub context: Option<Context>,
```

Where `Context` is an untagged `String | Vec<String>` (array joined with
`\n`). Absent/`null`/empty-string/empty-array all normalize to "no context".
Load tests mirror the existing `cascade`/`max_turns` field tests: parses a
string; parses an array; empty string and empty array load as no context;
no regression for tasks without the field.

**Render (`src/execute.rs`).** Extend `render_prompt`:

1. After the existing placeholder substitution, replace `{{CONTEXT}}` with
   the task's context (empty string when absent).
2. Replace `{{PREVIOUS_ERROR}}` with the failure-context block when it is
   `Some`; the existing append-at-end path stays as the fallback for
   templates without the token (and for attempts with no prior failures, the
   token resolves to the empty string).
3. Generalize the conditional-marker strip: `strip_scope_markers` becomes
   `strip_conditional_markers` and drops `{{#CONTEXT}}/{{/CONTEXT}}` lines
   (and their inline occurrences) exactly like `{{#SCOPE}}/{{/SCOPE}}`, and
   drops the whole section when the context is empty — so a template can
   have a context section that vanishes silently when unset.
4. Hardening parity: if a custom template omits `{{CONTEXT}}` AND the task
   has context, append a `## Task context` section (same pattern as the
   SCOPE/ACCEPTANCE/ACCEPT_CMD warnings) so crafted context can never be
   silently dropped by a template.

**Tests:** `tests/contract_prompt.rs` (pinned contract surface) gains:
context string renders into `{{CONTEXT}}`; array join; no-context renders
empty; `{{#CONTEXT}}…{{/CONTEXT}}` section drops when absent and renders when
present; `{{PREVIOUS_ERROR}}` substituted on retry renders vs appended on
first attempt; hostile template omitting `{{CONTEXT}}` is repaired and warned.

## D3 — `AF_PROMPT_FILE` env override (`src/config.rs`)

`Settings::from_env` reads `env_or("AF_PROMPT_FILE", "prompts/worker.md")`
for `prompt_file` — the `env_or` helper already applies the
`AF_*` → legacy `TF_*` fallback with the one-time deprecation notice. The
template file is re-read on every render (`render_prompt` reads
`st.prompt_file` per call), so editing a custom template mid-run already
takes effect on the next attempt — no caching change.

**Tests:** unit — `from_env` honors `AF_PROMPT_FILE`; falls back to
`TF_PROMPT_FILE` with warning; defaults to `prompts/worker.md`.

## D4 — Template cleanup (`prompts/worker.md`)

Reword the shipped template:
- Drop `{{EPISODES}}` and `{{MISSION}}` (ADR-13: cross-task recall deferred;
  per-task `context` is the mission).
- Keep and wire `{{PREVIOUS_ERROR}}` in the retry section; add a
  `{{#CONTEXT}}`/`{{/CONTEXT}}` conditional "Craft context" section so
  per-task context sits in the right part of the prompt.
- Required placeholders `{{SCOPE}}`, `{{ACCEPTANCE}}`, `{{ACCEPT_CMD}}` stay
  untouched (contract guarantee, `tests/contract_prompt.rs`).

## Structure

- `src/run.rs` — `pool_capacity`, mutable `max_parallel`, reload-branch recompute.
- `src/config.rs` — `Task.context` (+ `Context` type), `AF_PROMPT_FILE`.
- `src/execute.rs` — `{{CONTEXT}}`/`{{PREVIOUS_ERROR}}` substitution,
  conditional-marker generalization, append-fallback + hardening.
- `prompts/worker.md` — reworded template (drop EPISODES/MISSION, add context section).
- `tests/contract_prompt.rs`, `src/run.rs` tests, `src/config.rs` tests.
- `README.md` — task-schema `context` row; `AF_PROMPT_FILE` row in the env table.
- `docs/arc42/` — runtime view chapter (prompt rendering + hot reload) if the
  wording there names the placeholder set / parallelism derivation.

## Migration

None (both additions are optional). Existing `tasks.json` without `context`
and existing workloads without `AF_PROMPT_FILE` behave exactly as today —
the dead template tokens vanishing from `prompts/worker.md` is the only
user-visible diff, and it fixes a real leak (tokens were reaching agents
verbatim). Legacy `TF_PROMPT_FILE` continues to work with a deprecation
notice, per the shared fallback rule.
