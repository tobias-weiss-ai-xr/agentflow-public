# Troubleshooting af campaigns

Diagnosing an `af` campaign is a read-first exercise: nearly every question
("is it stuck?", "why did that task fail?", "where did my budget go?",
"can I salvage that branch?") is answered by the status board, the run log,
or the receipts under the state dir. This page covers the common failure
modes and the exact commands that answer them.

The state dir defaults to `state/` and can be moved with `AF_STATE_DIR`
(e.g. `state-self-improve/`, `state-piws/`). Substitute your state dir for
`<state>` throughout.

## First stop: `af status` and the run log

When a campaign looks wrong, **start here — always**:

```sh
af status          # per-task board: done / running / failed
tail -f <state>/campaign.log
```

- `af status` prints the per-task board with each task's state — `done`,
  `running`, `failed` — plus attempt counts and the state dir it consulted.
  Use `af status --json` when you want machine-readable output (it also
  exposes the latest receipt's outcome and `failure_class` per task).
- The run log at `<state>/campaign.log` carries the dispatch narrative:
  which worker took which task, retry announcements, gate/merge verdicts,
  and budget or deadlock messages. If you launched with `af run --detach`,
  the detached process redirects its output to `<state>/af-run.log` instead
  — same content, different file.
- Two quick cross-checks: a stale `af.pid` (from a `kill -9`) means no
  orchestrator is actually running, and a `stop` flag in the state dir means
  the last `af stop` asked the run loop to drain on its next poll.

If the board says `done` but the result is missing, check the receipt (next
section) before re-running anything — `af run` is crash-safe and resumes,
so a duplicate campaign is rarely the fix.

## Reading receipts: every attempt leaves a receipt

Every attempt — merged, failed, or infra-interrupted — writes a JSON
receipt to `<state>/receipts/`. Receipts are the durable record the router,
cost report, and status board are built from. Fields worth knowing:

| Field | Meaning |
|-------|---------|
| `worker` / `model` | who ran the attempt and with which model |
| `wall_clock_s` | measured wall-clock duration |
| `tokens` | token spend (with prompt/completion split when the provider reports it) |
| `outcome` | `merged`, `verified`, `failed`, `interrupted`, `recovered`, `spawned` |
| `failure_class` | on failures: `infra` or `agent` |

The `failure_class` split matters:

- **`agent`** — the work failed its gate/scope/merge. This is a verdict on
  the worker and lowers its trust in the UCB1 router.
- **`infra`** — provider errors, timeouts, stalls, missing binaries,
  transport/git/io failures. **An infra failure never lowers trust**: the
  infrastructure failed, not the worker, so it is excluded from trust
  statistics on both sides of the ratio while still counting toward cost.

`af cost` sums the receipts into a per-worker cost/trust report; unreadable
receipts are reported as warnings by `af status` rather than silently
dropped.

## Exit codes: deadlock (2) and budget (3)

`af run` exits non-zero in two well-defined situations; the run log names
the cause explicitly.

**Deadlock — exit code 2.** No task is ready (its dependencies gate it) but
work remains in scope: the DAG cannot make progress. Look for a dependency
cycle or a dependency that exhausted its own `max_attempts` and went
`failed`, starving its dependents.

**Budget exhaustion — exit code 3.** A campaign spend ceiling was hit:

- `AF_MAX_WALL_CLOCK_S` — wall-clock ceiling in seconds (`0` = unlimited)
- `AF_MAX_TOKENS` — token spend ceiling (`0` = unlimited)

Exit 3 **only stops NEW dispatches**. Attempts already running are allowed
to finish and their receipts land normally; nothing in flight is killed and
no state is lost. Re-running `af run` resumes from the atomic state once
the ceiling is raised or removed (`0` disables the cap).

## Retry prompts and rejected branches

A task that fails its gate does not fail the campaign outright:

- It **retries up to `max_attempts`** (per task, from `workers.json`
  defaults). Each retry gets a fresh branch and worktree.
- The **retry prompt lists the task's earlier failures** with their error
  reasons, so the agent does not repeat the same mistake. Gate-failure
  receipts carry the failing TAIL of the gate output (not the first banner
  line), so the agent is told why the gate rejected its work.
- **Repeated identical gate failure stops early**: when two independent
  agent-driven attempts die at the gate with byte-identical output (same
  exit code, same failing tail), the task is marked `failed` immediately
  with reason `repeated identical gate failure (…) — task design defect`,
  even if `max_attempts` has not been reached. This is a task-design
  defect (broken gate, wrong fixture, wrong command), not a flake — fix
  the gate or the task config rather than re-dispatching.
- When attempts are exhausted (or the defect detector stops early), the
  task is marked `failed` with the last error in `af status`; dependents
  see it and the run continues (or deadlocks with exit 2 if nothing else
  is ready).
- **Adaptive per-worker backoff**: a worker whose attempt fails as
  infrastructure (`failure_class: "infra"` — provider 4xx/5xx, rate
  limit, transport/harness timeout) is cooled down before it can be
  picked again, escalating 30 s → 60 s → 120 s → 240 s → 300 s ceiling.
  Any verdict (merged, verified, or non-infra failed) resets the
  escalation to level 0. A solo worker is capped at 30 s so the campaign
  keeps retrying rather than idling for minutes.

**Failed work is archived, never destroyed.** Every failure path — gate
failure, stall, timeout, non-zero exit, scope violation, merge conflict,
interrupted run — saves the attempt's branch as `<branch>-rejected-<ts>`
(with the attempt number when relevant). That work was already paid for,
and it is recoverable:

```sh
af recover --task ID [--attempt N] [--dry-run]
```

`af recover` re-validates the archived branch against the task's *current*
scope and gate, and merges it if it passes — the agent is never re-invoked.
The next `af run` also auto-reuses archived work when the scope still
matches, so a rejected branch can land itself on retry.

## Cleaning up after crashed runs

A `kill -9`, OOM, or lost machine leaves orphaned worktrees and branches
behind. Startup self-heal removes most of these, and `af clean` handles the
rest:

```sh
af clean --dry-run    # report what would go; change nothing
af clean              # actually sweep
```

`af clean` removes orphaned worktrees under the worktree root plus the
archived `<prefix>/<id>-rejected-<ts>[-<n>]` refs left by finished retries.
Tasks still marked `running` are preserved in both passes, so you can run
it against a live campaign without destroying in-flight work — but prefer
`af stop` first, then `af clean` once the drain completes. A stale
`af.pid` file is reported as a warning, never fatal.

## Quick reference

| Symptom | First check |
|---------|-------------|
| Board shows `running` forever | run log + receipt `wall_clock_s`; was the agent stalled or the process killed? |
| Task `failed`, seems fine to you | receipt `failure_class` (`agent` vs `infra`), then `af recover --task ID`; if the reason is `repeated identical gate failure` the gate or task config is broken — fix it, don't re-dispatch |
| Run exited 2 | deadlocked DAG: cycle or failed dependency |
| Run exited 3 | `AF_MAX_WALL_CLOCK_S` / `AF_MAX_TOKENS` ceiling hit; in-flight work was not killed |
| Leftover `af-*/` worktrees | `af clean --dry-run` |
| "Which campaign is this?" | `af status` names the state dir it consulted at the top of the board |
