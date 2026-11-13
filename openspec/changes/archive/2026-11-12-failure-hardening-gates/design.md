# Design: failure-hardening-gates

## Context

- Receipt `error` = first line of the failure reason, capped 200 chars
  (`src/execute.rs:157` inside `execute_task`). The full gate output is
  temporarily in the `Outcome::Failed` reason (built at `src/execute.rs:754`
  from `gate_out.combined().trim()`) before that truncation — so both the
  tail data and the truncation point already exist; only the selection
  changes.
- The run loop already routes infra failures away from verdict statistics
  AND applies a flat 30 s `worker_backoff` in `reap` (`src/run.rs:104`);
  `pick_worker` (`src/run.rs:155-185`) skips backed-off workers. The
  mechanics for C are present; only the value escalates.
- `reap` owns the retry decision (`Outcome::Failed` → `Ready` vs `Failed`),
  so B slots in at a single decision point with access to the fresh error
  and, via `store`, the task's prior receipts. Gate-only reruns can be
  distinguished from agent runs by their receipts (near-zero wall clock and
  no `turns`), and by the task's journal phase.

## Goals / Non-Goals

- Goals: retry memory carries the REAL gate failure; unfixable gate defects
  stop paying; infra failures stop hammering; drift is visible per worker.
- Non-Goals: changing verdict math, retry budgets, gate execution, or adding
  network probes (all stated in proposal).

## A — Gate-failure tail on the receipt (`src/execute.rs`, `src/state.rs`)

- Keep the `Outcome::Failed` reason's prefix `acceptance gate failed (exit
  N): ` verbatim (it feeds `waste_cause` grouping and the gate-only retry
  heuristics in `run.rs`).
- After the reason's colon, for gate failures store the **tail**: a helper
  `gate_error_tail(combined: &str) -> String` returns the last line (or up
  to the last 200 chars, folding to whole lines) of the gate's combined
  output, or the marker `(gate produced no output; exit N)` when empty.
- Implement at the single truncation site (`execute.rs:157`): detect the
  `acceptance gate failed` prefix; if present, take the tail; otherwise
  keep the current first-line rule. `execute_task` is pure/testable — the
  tests in `execute.rs` and `tests/contract_*` cover both branches.
- Consequence without extra code: ADR-13 `failure_context` and `af status`
  read `Receipt.error`, so the tail automatically reaches retry prompts and
  the status board. `af cost` by-reason grouping sees the same prefix and
  unchanged grouping semantics.

Edge: a gate that prints a huge warning banner then a one-line failure —
the tail picks the failure line. A gate that prints nothing and returns the
silent exit-code path — the marker names it. A gate-only rerun (same
committed work) producing the same tail is fine and expected.

## B — Repeated identical gate failure stops early (`src/run.rs:reap`)

State-less decision in `reap` using persisted receipts (survives
resume):

1. **Fingerprint** of a gate failure: tuple `(exit_code, whitespace-
   collapsed tail)` derived from the `error` string by the same normalizer
   D-cases use (trim, collapse runs of whitespace, lowercase for the exit
   code text). Pure function: `gate_fingerprint(&str) -> Option<(String,
   String)>`; `None` for non-gate errors.
2. On reaping an `Outcome::Failed(err)` whose fingerprint is `Some`:
   - load the task's receipts (cache already maintained by the loop —
     reuse `all_receipts_cache` in the main loop, or `store.load_receipts`
     in `--once`), filter the SAME task's earlier `failed` gate receipts.
   - walk back from the most recent: skip receipts that look like gate-only
     reruns (wall_clock_s < 5 s AND turns absent/0 — heuristic, documented
     in the code); the first prior AGENT-driven gate receipt's fingerprint
     is the comparison target.
   - if the fingerprints match AND `s.attempts >= 2`: set
     `s.state = TaskState::Failed`; `s.last_error = "repeated identical
     gate failure (<tail>) — task design defect"`; persist; print the
     stop with its reason; do NOT dispatch further attempts (the normal
     `max_attempts` check is bypassed only for the EARLY stop).
   - a new fingerprint (or no prior agent-driven gate receipt) falls
     through to the normal retry decision unchanged.
3. Gate-only reruns never trigger (they carry no agent work — comparing
   them would stop every broken-gate task at attempt 2, which is
   actually correct, but the guard keeps the rule honest: two INDEPENDENT
   agent attempts must agree).

Rationale for "no per-task opt-out in v1": two independent agent runs
producing byte-identical gate output is an overwhelming deterministic
defect signal; the residual risk (a nondeterministic gate that repeats) is
an operator-visible early stop, recoverable by re-running with a fixed
gate — better than burning three more agents.

## C — Adaptive per-worker backoff (`src/run.rs:104`, plus a small store)

- Replace the flat `+30s` insert with a helper holding escalation state:
  `worker_backoff` becomes `HashMap<String, BackoffState>` where
  `BackoffState { until: Instant, level: u32 }`; level = number of
  consecutive infra failures observed.
- On infra failure: `level += 1`, `until = now + min(30 << (level-1),
  300)`. On ANY verdict (merged/verified/non-infra failed) for that
  worker: `level = 0` (and `until` cleared).
- Guard: when `cfg.workers` has exactly one enabled worker, cap `until` at
  `now + 30s` regardless of level — never lock out the only pool member
  for 5 minutes (the loop would otherwise idle with `running` empty and
  `ready > 0`, and while that is not a deadlock exit, a solo campaign must
  keep trying).
- `pick_worker` unchanged (it already filters `backoff.get(name) > now`).
  The map type change is internal.

## D — Infra-drift column in `af cost` (`src/run.rs` cost report)

- In the per-worker trust table, derive per worker (from the SAME selected
  window): `verdicts` (current `w/n`), plus `infra = number of selected
  failed receipts with `failure_class == "infra"` for that worker`, plus
  `last_infra = the most recent such receipt's error (capped 200)`.
- Render: append an `INFRA` column after `MEAN_S`; when `infra > 0` add a
  footnote line `infra: w1 (3): <last error>` — sorted, one per worker,
  never truncating mid-word, hidden when `infra == 0` for all workers (so
  an all-healthy report is byte-identical to today).
- Tokens/cost cells are untouched; column math only adds.

## Structure

- `src/state.rs` — (A) none; error content is decided in `execute.rs`.
- `src/execute.rs` — (A) `gate_error_tail` + truncation-site branch +
  unit tests (incl. the empty-output marker).
- `src/run.rs` — (B) `gate_fingerprint`, prior-agent-gate lookup + early
  stop in `reap`, unit tests with synthetic receipts; (C) BackoffState +
  escalation/reset + single-worker guard, unit tests; (D) INFRA column +
  footnote, unit tests on the report builder.
- `tests/contract_prompt.rs` — retry-prompt tail assertion (A leaks into
  `failure_context`); `tests/ledger_truth.rs`/`token_cost.rs` — fingerprint
  normalization + cost-report column.
- `docs/arc42/` — runtime view (gate failure diagnosis + early stop).

## Migration

None: A changes only what a gate-failure receipt *says* (a strict
improvement for retry memory); B stops tasks EARLY, which strictly reduces
spend and only changes the final reason a task shows after repeated
identical failures; C changes cooldown *durations* only; D adds a column +
footnote to an existing report. Existing campaigns without gate failures,
infra failures, or backoff pressure render and behave unchanged.
