# Proposal: token-budget-ceiling

## Why

agentflow enforces a **wall-clock** spend ceiling (`AF_MAX_WALL_CLOCK_S`)
but has no **token** ceiling. The receipt ledger already records `tokens`
per attempt (first-class data since the native harness and the JSON
transcript parser), and `af cost` aggregates them — but nothing stops
dispatch when the token spend exceeds a cap. The "Token Budgets" paper
(arXiv 2606.04056) catalogs exactly this failure class: a single retry
loop spends thousands of dollars before an operator notices, because the
in-process integrity property that would prevent it is enforced ad hoc or
not at all. agentflow has the ledger; the ceiling is the missing piece.

A per-task ceiling is also missing: a task can burn unbounded tokens on
retries, and the only backstop is `max_turns` (which caps LLM round-trips,
not tokens). A task that hits the token ceiling should be stopped before
dispatch, exactly as `max_turns` caps the harness loop.

## What Changes

- **Campaign-level token ceiling** (`AF_MAX_TOKENS`, env var, default 0 =
  unlimited): stops NEW dispatches once the receipts for in-scope tasks
  total at least this many tokens. In-flight attempts finish. The run
  exits 3 (the same exit code as the wall-clock ceiling) with a diagnostic
  naming the spend, the cap, and the count of not-started tasks. The
  receipts are the ledger (survive restarts, compose across runs), exactly
  like the wall-clock ceiling.
- **Per-task token ceiling** (`max_tokens` in `tasks.json`, optional, 0 =
  no per-task cap): checked before dispatch. A task whose in-scope receipts
  already total at least its `max_tokens` is not dispatched and is marked
  `failed` with a reason naming the cap and the spend. This composes with
  the campaign ceiling and with `max_turns`/`max_attempts`.
- **`af cost` shows the token total** (already does via the `TOKENS`
  column); no report change needed. The ceiling is enforced against the
  same `select_receipts` window the report uses, so the enforced number
  is the number the operator sees.

## Capabilities

- **New Capabilities**:
  - `openspec/specs/scheduling/spec.md` — campaign token ceiling
    (`AF_MAX_TOKENS`) stops new dispatches at the cap; per-task `max_tokens`
    stops a single task from exceeding its token budget.
- **Modified Capabilities**:
  - `openspec/specs/config/spec.md` — `Settings` gains `max_tokens` (env
    `AF_MAX_TOKENS`); `Task` gains `max_tokens` (optional, 0 = no cap).
  - `openspec/specs/state/spec.md` — no change (receipts already carry
    `tokens`; the ceiling reads them).

## Impact

- `src/config.rs`: `Settings::max_tokens` + `Task::max_tokens` field +
  `af_var` read + validation (reject negative).
- `src/run.rs`: `consumed_tokens` (parallel to `consumed_wall_clock_s`),
  `token_budget_exhausted`, per-task check before dispatch, campaign-level
  check in the same block as `over_budget`.
- `tests/budget.rs` (existing wall-clock tests): new tests for the token
  ceiling — campaign-level stop, per-task stop, 0 = unlimited, composition
  with wall-clock.
- `README.md`: document `AF_MAX_TOKENS` and `max_tokens` alongside
  `AF_MAX_WALL_CLOCK_S` and `max_turns`.
- No new dependencies; the receipt ledger already exists.

## Design decisions

- **0 = unlimited** mirrors `AF_MAX_WALL_CLOCK_S` exactly. A ceiling of 0
  means "no opinion", preserving every existing config.
- **Per-task `max_tokens` is a dispatch gate, not a mid-attempt kill.**
  The harness loop has no token counter (the provider reports usage
  after the round-trip, not before); a mid-attempt kill would need a
  streaming token counter, which is a separate feature. The per-task
  ceiling stops the NEXT attempt from dispatching once the cap is reached,
  like `max_attempts` stops after N tries.
- **The ceiling reads verdict + interrupted receipts.** An interrupted
  attempt's `tokens` is `None` (no agent ran), so it contributes 0 — but
  a `failed` attempt that burned tokens DOES count, because the tokens
  were spent. This matches the wall-clock ceiling, which counts
  `wall_clock_s` on every receipt including failed ones.
- **Per-task ceiling marks the task `failed`, not `blocked`.** A task
  over its token budget has exhausted its allocation; `failed` with a
  clear reason is the honest status (the task did not succeed), and a
  future run with a higher cap or more tokens can retry it. There is no
  `blocked` state in agentflow today, and adding one is a separate
  concern.
